use rustle_core::Settings;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub(crate) async fn summarise(settings: &Settings, transcript: &str) -> Result<String, String> {
    if transcript.trim().is_empty() {
        return Err("transcript is empty".to_owned());
    }

    let endpoint = parse_endpoint(&settings.ai.ollama_url)?;
    let prompt = format!(
        "{}\n\nReturn Markdown with exactly these headings: Summary, Key Decisions, Action Items, Attendees. Keep each section concise and factual. Do not infer topics, decisions, attendees, or action items that are not explicitly present in the transcript. Ignore icebreakers, jokes, setup chatter, and social examples unless they create a real decision or action item. Use 'None captured.' for Key Decisions or Action Items when none are explicit. Do not include any Transcript section or transcript breakdown.\n\nTranscript:\n{}",
        settings.ai.system_prompt,
        transcript.trim()
    );
    let body = json!({
        "model": settings.ai.model,
        "prompt": prompt,
        "stream": false,
    })
    .to_string();

    let mut stream = TcpStream::connect(format!("{}:{}", endpoint.host, endpoint.port))
        .await
        .map_err(|error| error.to_string())?;
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        endpoint.path,
        endpoint.host,
        body.len(),
        body
    );

    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| error.to_string())?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .await
        .map_err(|error| error.to_string())?;

    let body = response
        .split("\r\n\r\n")
        .nth(1)
        .ok_or_else(|| "Ollama response did not include an HTTP body".to_owned())?;
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| format!("invalid Ollama JSON: {error}"))?;

    value
        .get("response")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|markdown| !markdown.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "Ollama response did not include generated text".to_owned())
}

fn parse_endpoint(base_url: &str) -> Result<OllamaEndpoint, String> {
    let stripped = base_url
        .trim()
        .strip_prefix("http://")
        .ok_or_else(|| "only http:// Ollama endpoints are supported".to_owned())?;
    let host_port = stripped.split('/').next().unwrap_or(stripped);
    let mut parts = host_port.splitn(2, ':');
    let host = parts
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Ollama endpoint is missing a host".to_owned())?
        .to_owned();
    let port = parts
        .next()
        .map(|value| value.parse::<u16>().map_err(|error| error.to_string()))
        .transpose()?
        .unwrap_or(11434);

    Ok(OllamaEndpoint {
        host,
        port,
        path: "/api/generate".to_owned(),
    })
}

struct OllamaEndpoint {
    host: String,
    port: u16,
    path: String,
}
