use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustle_core::{default_data_dir, expand_tilde, AiProvider, Settings, TranscriptionMethod};
use sha2::{Digest, Sha256};

pub(crate) const WHISPER_SHA: &str =
    "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002";
pub(crate) const NOTES_SHA: &str =
    "626b4a6678b86442240e33df819e00132d3ba7dddfe1cdc4fbb18e0a9615c62d";

pub(crate) fn check_cpu() -> anyhow::Result<String> {
    #[cfg(target_arch = "x86_64")]
    let supported = std::is_x86_feature_detected!("avx2")
        && std::is_x86_feature_detected!("fma")
        && std::is_x86_feature_detected!("f16c")
        && std::is_x86_feature_detected!("bmi2")
        && std::is_x86_feature_detected!("sse4.2");
    #[cfg(not(target_arch = "x86_64"))]
    let supported = false;
    anyhow::ensure!(supported, "local inference requires an x86-64 AVX2 CPU with FMA, F16C, BMI2 and SSE4.2 (Haswell/Zen or newer)");
    Ok("x86-64 local-inference CPU supported".to_owned())
}

pub(crate) fn sha256(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

async fn download(url: &str, path: &Path, expected: &str) -> anyhow::Result<()> {
    let check_path = path.to_owned();
    let expected_owned = expected.to_owned();
    if tokio::task::spawn_blocking(move || sha256(&check_path).is_ok_and(|s| s == expected_owned))
        .await?
    {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("model path has no parent"))?;
    tokio::fs::create_dir_all(parent).await?;
    let pending = path.with_extension(format!("{}.part", uuid::Uuid::new_v4()));
    tracing::info!(path = %path.display(), "downloading model; readiness waits for checksum and inference validation");
    let result: anyhow::Result<()> = async {
        let status = tokio::time::timeout(
            Duration::from_secs(1900),
            tokio::process::Command::new("curl")
                .args([
                    "--fail",
                    "--location",
                    "--proto",
                    "=https",
                    "--proto-redir",
                    "=https",
                    "--connect-timeout",
                    "15",
                    "--max-time",
                    "1800",
                    "--retry",
                    "2",
                    "--silent",
                    "--show-error",
                    "--output",
                ])
                .arg(&pending)
                .arg(url)
                .kill_on_drop(true)
                .status(),
        )
        .await??;
        anyhow::ensure!(
            status.success(),
            "model download failed: {status}; check network and run rustle setup again"
        );
        let check_path = pending.clone();
        let actual = tokio::task::spawn_blocking(move || sha256(&check_path)).await??;
        anyhow::ensure!(
            actual == expected,
            "model checksum mismatch; refusing partial or untrusted download"
        );
        tokio::fs::rename(&pending, path).await?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&pending).await;
    }
    result
}

pub(crate) async fn prepare(settings: &mut Settings) -> anyhow::Result<()> {
    check_cpu()?;
    anyhow::ensure!(
        settings.transcription.method == TranscriptionMethod::Local,
        "select local Whisper transcription"
    );
    let defaults = Settings::default();
    let directory = default_data_dir()?.join("models");
    let whisper = if settings.transcription.model_path == defaults.transcription.model_path {
        directory.join("ggml-base.en.bin")
    } else {
        expand_tilde(&settings.transcription.model_path)?
    };
    if settings.transcription.model_path == defaults.transcription.model_path
        || whisper == directory.join("ggml-base.en.bin")
    {
        download("https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.en.bin", &whisper, WHISPER_SHA).await?;
    }
    anyhow::ensure!(
        whisper.is_file(),
        "Whisper model missing at {}",
        whisper.display()
    );
    settings.transcription.model_path = whisper.to_string_lossy().into_owned();
    match settings.ai.provider {
        AiProvider::LlamaCpp => {
            let model = if settings.ai.model_path.is_empty() {
                anyhow::ensure!(settings.ai.hf_repo == defaults.ai.hf_repo && settings.ai.hf_model_file == defaults.ai.hf_model_file,
                    "place your custom model locally and set ai.model_path; automatic verified downloads support the default model");
                directory.join("qwen2.5-3b-instruct-q4_k_m.gguf")
            } else {
                expand_tilde(&settings.ai.model_path)?
            };
            if model == directory.join("qwen2.5-3b-instruct-q4_k_m.gguf") {
                download("https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/7dabda4d13d513e3e842b20f0d435c732f172cbe/qwen2.5-3b-instruct-q4_k_m.gguf", &model, NOTES_SHA).await?;
            }
            anyhow::ensure!(
                model.is_file(),
                "notes model missing at {}",
                model.display()
            );
            settings.ai.model_path = model.to_string_lossy().into_owned();
        }
        AiProvider::Ollama => {}
        _ => anyhow::bail!("select llama_cpp or ollama; hosted notes providers are unsupported"),
    }
    Ok(())
}

pub(crate) fn paths(settings: &Settings) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(path) = expand_tilde(&settings.transcription.model_path) {
        paths.push(path);
    }
    if settings.ai.provider == AiProvider::LlamaCpp {
        if let Ok(path) = expand_tilde(&settings.ai.model_path) {
            paths.push(path);
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checksum_detects_corruption() {
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::write(&path, b"abcd").unwrap();
        assert_ne!(
            sha256(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(path).unwrap();
    }
}
