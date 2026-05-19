use rustle_core::Settings;

#[cfg(feature = "provider-llama-cpp")]
use std::sync::OnceLock;

#[cfg(feature = "provider-llama-cpp")]
static LLAMA_BACKEND: OnceLock<Result<llama_cpp_2::llama_backend::LlamaBackend, String>> =
    OnceLock::new();

pub(crate) fn warm_up(_settings: &Settings) -> Result<(), String> {
    #[cfg(feature = "provider-llama-cpp")]
    {
        let _ = backend()?;
    }

    Ok(())
}

pub(crate) async fn summarise(settings: &Settings, transcript: &str) -> Result<String, String> {
    #[cfg(feature = "provider-llama-cpp")]
    {
        let settings = settings.clone();
        let transcript = transcript.to_owned();
        tokio::task::spawn_blocking(move || summarise_blocking(&settings, &transcript))
            .await
            .map_err(|error| error.to_string())?
    }

    #[cfg(not(feature = "provider-llama-cpp"))]
    {
        let _ = (settings, transcript);
        provider_status()
    }
}

#[cfg_attr(feature = "provider-llama-cpp", allow(dead_code))]
fn provider_status() -> Result<String, String> {
    #[cfg(feature = "provider-llama-cpp")]
    {
        Err("the llama-cpp-2 provider feature is enabled, but llama.cpp inference failed to initialize".to_owned())
    }

    #[cfg(not(feature = "provider-llama-cpp"))]
    {
        Err(
            "the llama-cpp-2 provider requires rebuilding with `--features provider-llama-cpp`"
                .to_owned(),
        )
    }
}

#[cfg(feature = "provider-llama-cpp")]
fn summarise_blocking(settings: &Settings, transcript: &str) -> Result<String, String> {
    use std::num::NonZeroU32;

    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::params::LlamaModelParams;
    use llama_cpp_2::model::LlamaModel;
    use llama_cpp_2::sampling::LlamaSampler;
    use llama_cpp_2::{send_logs_to_tracing, LogOptions};

    const SAMPLE_LEN: i32 = 128;

    if transcript.trim().is_empty() {
        return Err("transcript is empty".to_owned());
    }

    let user_prompt = format!(
        "Create meeting notes from this transcript. Return Markdown with exactly these headings: Summary, Key Decisions, Action Items, Attendees, Transcript. Keep each section concise and factual.\n\nTranscript:\n{}",
        transcript.trim()
    );

    send_logs_to_tracing(LogOptions::default().with_logs_enabled(false));

    let backend = backend()?;
    let model_path = model_path(settings)?;
    let model = LlamaModel::load_from_file(backend, model_path, &LlamaModelParams::default())
        .map_err(|error| error.to_string())?;
    let prompt = render_prompt(&model, settings.ai.system_prompt.trim(), &user_prompt)?;

    let n_ctx = NonZeroU32::new(2048).ok_or_else(|| "invalid llama.cpp context size".to_owned())?;
    let ctx_params = LlamaContextParams::default().with_n_ctx(Some(n_ctx));
    let mut ctx = model
        .new_context(backend, ctx_params)
        .map_err(|error| error.to_string())?;

    let prompt_tokens = model
        .str_to_token(&prompt, llama_cpp_2::model::AddBos::Never)
        .map_err(|error| error.to_string())?;
    let target_len =
        i32::try_from(prompt_tokens.len()).map_err(|error| error.to_string())? + SAMPLE_LEN;

    let mut batch = LlamaBatch::new(512, 1);
    let last_index =
        i32::try_from(prompt_tokens.len().saturating_sub(1)).map_err(|error| error.to_string())?;
    for (index, token) in (0_i32..).zip(prompt_tokens.into_iter()) {
        batch
            .add(token, index, &[0], index == last_index)
            .map_err(|error| error.to_string())?;
    }
    ctx.decode(&mut batch).map_err(|error| error.to_string())?;

    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut sampler = LlamaSampler::chain_simple([
        LlamaSampler::penalties(64, 1.15, 0.0, 0.0),
        LlamaSampler::top_k(40),
        LlamaSampler::top_p(0.9, 1),
        LlamaSampler::temp(0.7),
        LlamaSampler::dist(1234),
    ]);
    let mut output = String::new();
    let mut current_tokens = batch.n_tokens();

    while current_tokens <= target_len {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);

        if model.is_eog_token(token) {
            break;
        }

        let piece = model
            .token_to_piece(token, &mut decoder, true, None)
            .map_err(|error| error.to_string())?;
        output.push_str(&piece);

        batch.clear();
        batch
            .add(token, current_tokens, &[0], true)
            .map_err(|error| error.to_string())?;
        ctx.decode(&mut batch).map_err(|error| error.to_string())?;
        current_tokens += 1;
    }

    if output.trim().is_empty() {
        return Err("llama.cpp generation produced no text".to_owned());
    }

    if looks_degenerate(&output) {
        return Err("llama.cpp generation produced degenerate repetitive output".to_owned());
    }

    let markdown = strip_outer_code_fences(&output);
    tracing::debug!(
        model = settings.ai.model.trim(),
        "llama.cpp generated markdown candidate:\n```markdown\n{}\n```",
        markdown.trim()
    );

    normalise_markdown(&markdown)
}

#[cfg(feature = "provider-llama-cpp")]
fn model_path(settings: &Settings) -> Result<std::path::PathBuf, String> {
    use hf_hub::api::sync::Api;

    if !settings.ai.model_path.trim().is_empty() {
        return Ok(std::path::PathBuf::from(settings.ai.model_path.trim()));
    }

    let repo = if !settings.ai.hf_repo.trim().is_empty() {
        settings.ai.hf_repo.trim()
    } else if settings.ai.model.contains('/') {
        settings.ai.model.trim()
    } else {
        return Err("llama.cpp requires `ai.model_path` or Hugging Face repo metadata".to_owned());
    };

    let file = settings.ai.hf_model_file.trim();
    if file.is_empty() {
        return Err(
            "llama.cpp requires `ai.hf_model_file` when downloading from Hugging Face".to_owned(),
        );
    }

    if !repo.contains('/') {
        return Err(format!("invalid Hugging Face model repo `{repo}`"));
    }
    let client = Api::new().map_err(|error| error.to_string())?;

    client
        .model(repo.to_owned())
        .get(file)
        .map_err(|error| error.to_string())
}

#[cfg(feature = "provider-llama-cpp")]
fn backend() -> Result<&'static llama_cpp_2::llama_backend::LlamaBackend, String> {
    LLAMA_BACKEND
        .get_or_init(|| {
            llama_cpp_2::llama_backend::LlamaBackend::init().map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

#[cfg(feature = "provider-llama-cpp")]
fn render_prompt(
    model: &llama_cpp_2::model::LlamaModel,
    system_prompt: &str,
    user_prompt: &str,
) -> Result<String, String> {
    let template = model
        .chat_template(None)
        .map_err(|error| error.to_string())?;
    let messages = [
        llama_cpp_2::model::LlamaChatMessage::new("system".to_owned(), system_prompt.to_owned())
            .map_err(|error| error.to_string())?,
        llama_cpp_2::model::LlamaChatMessage::new("user".to_owned(), user_prompt.to_owned())
            .map_err(|error| error.to_string())?,
    ];

    model
        .apply_chat_template(&template, &messages, true)
        .map_err(|error| error.to_string())
}

#[cfg(feature = "provider-llama-cpp")]
fn looks_degenerate(output: &str) -> bool {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() < 4 {
        return false;
    }

    let repeated_pairs = lines
        .windows(2)
        .filter(|window| window[0] == window[1])
        .count();
    if repeated_pairs >= 2 {
        return true;
    }

    let unique = lines
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    unique.len() * 3 <= lines.len()
}

#[cfg(feature = "provider-llama-cpp")]
fn normalise_markdown(output: &str) -> Result<String, String> {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed == "markdown" {
        return Err("llama.cpp generation produced empty markdown".to_owned());
    }

    let normalised = normalise_section_headings(trimmed);
    let normalised = ensure_title(&normalised);

    if is_acceptable_markdown(&normalised) {
        return Ok(normalised);
    }

    Err("llama.cpp generation produced invalid markdown structure".to_owned())
}

#[cfg(feature = "provider-llama-cpp")]
fn normalise_section_headings(markdown: &str) -> String {
    markdown
        .lines()
        .map(|line| match canonical_section_name(line) {
            Some(section) => format!("## {section}"),
            None => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(feature = "provider-llama-cpp")]
fn canonical_section_name(line: &str) -> Option<&'static str> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let trimmed = trimmed.trim_start_matches('#').trim();
    let trimmed = trimmed.strip_prefix("**").unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix("**").unwrap_or(trimmed);
    let trimmed = trimmed.strip_prefix("__").unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix("__").unwrap_or(trimmed);
    let trimmed = trimmed.trim_end_matches(':').trim();

    match trimmed.to_ascii_lowercase().as_str() {
        "summary" => Some("Summary"),
        "key decisions" => Some("Key Decisions"),
        "action items" => Some("Action Items"),
        "attendees" => Some("Attendees"),
        "transcript" => Some("Transcript"),
        _ => None,
    }
}

#[cfg(feature = "provider-llama-cpp")]
fn ensure_title(markdown: &str) -> String {
    let trimmed = markdown.trim();
    if trimmed.starts_with("# Meeting Notes") {
        trimmed.to_owned()
    } else {
        format!("# Meeting Notes\n\n{trimmed}")
    }
}

#[cfg(feature = "provider-llama-cpp")]
fn is_acceptable_markdown(markdown: &str) -> bool {
    let section_count = [
        "## Summary",
        "## Key Decisions",
        "## Action Items",
        "## Attendees",
        "## Transcript",
    ]
    .iter()
    .filter(|section| markdown.contains(**section))
    .count();

    let has_content_line = markdown
        .lines()
        .map(str::trim)
        .any(|line| !line.is_empty() && !line.starts_with('#'));

    has_content_line && (section_count >= 2 || markdown.starts_with("# Meeting Notes\n\n"))
}

#[cfg(feature = "provider-llama-cpp")]
fn strip_outer_code_fences(output: &str) -> String {
    let trimmed = output.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed.to_owned();
    };
    let rest = rest.trim_start_matches(|character| character != '\n');
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let body = rest.strip_suffix("```").unwrap_or(rest);
    body.trim().to_owned()
}

#[cfg(all(test, feature = "provider-llama-cpp"))]
mod tests {
    use super::normalise_markdown;

    #[test]
    fn accepts_bold_section_labels_from_model_output() {
        let markdown = normalise_markdown(
            "**Summary**\nA short summary.\n\n**Key Decisions**\n- Keep the manual transcript flow.",
        )
        .expect("markdown should be accepted");

        assert!(markdown.starts_with("# Meeting Notes"));
        assert!(markdown.contains("## Summary"));
        assert!(markdown.contains("## Key Decisions"));
        assert!(markdown.contains("A short summary."));
    }
}
