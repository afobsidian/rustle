use std::fmt;
use std::time::Duration;
use std::time::Instant;

use rustle_core::{AiProvider, Settings};
use tokio::time::timeout;

use crate::providers::{AiRuntime, SummarisationOutcome};

const BENCHMARK_TRANSCRIPT: &str = "Alex: We agreed to keep the manual transcript flow for the MVP while automatic capture is still being built.\nPriya: The main risk is that users may not know when summarisation is happening, so progress logging should stay visible.\nSam: I will verify the generated note format and make sure action items are easy to scan.";
const DEFAULT_BENCHMARK_TIMEOUT_SECS: u64 = 30;

struct LlamaCppBenchmarkModel {
    repo: &'static str,
    file: &'static str,
}

pub async fn run_provider_benchmark() -> Vec<ProviderBenchmarkResult> {
    let mut results = Vec::new();
    for model in benchmark_models() {
        results.push(benchmark_provider(&model).await);
    }

    results
}

pub async fn run_provider_benchmark_with_progress(
    mut on_progress: impl FnMut(BenchmarkProgress<'_>),
) -> Vec<ProviderBenchmarkResult> {
    let mut results = Vec::new();
    for model in benchmark_models() {
        on_progress(BenchmarkProgress::Starting(model.repo));
        let result = benchmark_provider(&model).await;
        on_progress(BenchmarkProgress::Finished(&result));
        results.push(result);
    }

    results
}

async fn benchmark_provider(model: &LlamaCppBenchmarkModel) -> ProviderBenchmarkResult {
    let mut settings = Settings::default();
    settings.ai.provider = AiProvider::LlamaCpp;
    settings.ai.model = model.repo.to_owned();
    settings.ai.hf_repo = model.repo.to_owned();
    settings.ai.hf_model_file = model.file.to_owned();
    settings.ai.model_path.clear();

    let mut runtime = AiRuntime::default();
    let warmup_started = Instant::now();
    runtime.warm_up(&settings);
    let warmup_ms = warmup_started.elapsed().as_millis();

    let timeout_duration = benchmark_timeout();
    let generation_started = Instant::now();
    let outcome = timeout(
        timeout_duration,
        runtime.summarise(&settings, BENCHMARK_TRANSCRIPT),
    )
    .await;
    let generation_ms = generation_started.elapsed().as_millis();

    let (status, output_chars, preview) = match outcome {
        Ok(SummarisationOutcome::Ready(notes)) => {
            let status = if notes.summary == "Summary unavailable" {
                BenchmarkStatus::Fallback
            } else {
                BenchmarkStatus::Ready
            };
            (
                status,
                notes.markdown.chars().count(),
                preview(&notes.markdown),
            )
        }
        Err(_) => (
            BenchmarkStatus::TimedOut,
            0,
            format!(
                "benchmark timed out after {}s; set RUSTLE_BENCH_TIMEOUT_SECS to raise the limit",
                timeout_duration.as_secs()
            ),
        ),
    };
    let model_source = model_source(&settings);

    ProviderBenchmarkResult {
        provider: AiProvider::LlamaCpp,
        model: settings.ai.model,
        model_source,
        warmup_ms,
        generation_ms,
        output_chars,
        status,
        preview,
    }
}

fn benchmark_timeout() -> Duration {
    let secs = std::env::var("RUSTLE_BENCH_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_BENCHMARK_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

fn benchmark_models() -> Vec<LlamaCppBenchmarkModel> {
    vec![
        LlamaCppBenchmarkModel {
            repo: "bartowski/SmolLM2-360M-Instruct-GGUF",
            file: "SmolLM2-360M-Instruct-Q4_K_M.gguf",
        },
        LlamaCppBenchmarkModel {
            repo: "Qwen/Qwen2.5-0.5B-Instruct-GGUF",
            file: "qwen2.5-0.5b-instruct-q4_k_m.gguf",
        },
        LlamaCppBenchmarkModel {
            repo: "bartowski/Llama-3.2-1B-Instruct-GGUF",
            file: "Llama-3.2-1B-Instruct-IQ3_M.gguf",
        },
        LlamaCppBenchmarkModel {
            repo: "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
            file: "qwen2.5-1.5b-instruct-q2_k.gguf",
        },
    ]
}

fn model_source(settings: &Settings) -> String {
    if !settings.ai.model_path.trim().is_empty() {
        return format!("path:{}", settings.ai.model_path);
    }
    if !settings.ai.hf_repo.trim().is_empty() {
        if !settings.ai.hf_model_file.trim().is_empty() {
            return format!("hf:{}#{}", settings.ai.hf_repo, settings.ai.hf_model_file);
        }
        return format!("hf:{}", settings.ai.hf_repo);
    }

    match settings.ai.provider {
        AiProvider::Ollama => settings.ai.ollama_url.clone(),
        _ => "provider-default".to_owned(),
    }
}

fn preview(markdown: &str) -> String {
    markdown
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("No preview available.")
        .chars()
        .take(120)
        .collect()
}

pub enum BenchmarkProgress<'a> {
    Starting(&'a str),
    Finished(&'a ProviderBenchmarkResult),
}

#[derive(Debug)]
pub struct ProviderBenchmarkResult {
    pub provider: AiProvider,
    pub model: String,
    pub model_source: String,
    pub warmup_ms: u128,
    pub generation_ms: u128,
    pub output_chars: usize,
    pub status: BenchmarkStatus,
    pub preview: String,
}

#[derive(Debug)]
pub enum BenchmarkStatus {
    Ready,
    Fallback,
    TimedOut,
}

impl fmt::Display for BenchmarkStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready => formatter.write_str("ready"),
            Self::Fallback => formatter.write_str("fallback"),
            Self::TimedOut => formatter.write_str("timed-out"),
        }
    }
}
