use rustle_core::{default_config_path, expand_tilde, Settings};

/// Exercise a separate installed process with isolated storage and virtual audio devices.
/// The same harness is used by first-run setup and the desktop release gate.
pub(crate) async fn run() -> anyhow::Result<()> {
    crate::models::check_cpu()?;
    let mut settings = Settings::load().await?;
    settings.ai.api_key.clear();
    settings.transcription.openai_api_key.clear();
    settings.transcription.model_path = expand_tilde(&settings.transcription.model_path)?
        .to_string_lossy()
        .into_owned();
    anyhow::ensure!(
        !settings.ai.model_path.is_empty()
            || settings.ai.provider == rustle_core::AiProvider::Ollama,
        "run rustle setup to provision and verify the local notes model first"
    );
    if !settings.ai.model_path.is_empty() {
        settings.ai.model_path = expand_tilde(&settings.ai.model_path)?
            .to_string_lossy()
            .into_owned();
    }
    let base = std::env::var_os("RUSTLE_TEST_ARTIFACTS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let work = base.join(format!("rustle-self-test-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&work).await?;
    let work = tokio::fs::canonicalize(&work).await?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let microphone = format!("rustle_test_mic_{id}");
    let output = format!("rustle_test_output_{id}");
    settings.general.start_on_login = false;
    settings.meeting.auto_detect = std::env::var_os("RUSTLE_TEST_TEAMS_SERVICE").is_some();
    settings.meeting.auto_capture = true;
    settings.audio.capture_loopback = true;
    settings.audio.input_device = format!("{microphone}.monitor");
    settings.audio.output_device = "default".to_owned();
    settings.audio.max_recording_size_mb = 1;
    settings.storage.notes_dir = work
        .join("data/rustle/notes")
        .to_string_lossy()
        .into_owned();
    settings
        .save_to_path(work.join("config/rustle/config.toml"))
        .await?;
    tokio::fs::write(
        work.join("microphone.wav"),
        include_bytes!("../assets/self-test/microphone.wav"),
    )
    .await?;
    tokio::fs::write(
        work.join("output.wav"),
        include_bytes!("../assets/self-test/output.wav"),
    )
    .await?;
    let status = tokio::process::Command::new("dbus-run-session")
        .args(["--", "python3"])
        .arg("-c")
        .arg(include_str!("../scripts/self-test.py"))
        .arg(std::env::current_exe()?)
        .arg(&work)
        .arg(&microphone)
        .arg(&output)
        .env_remove("RUSTLE_TEST_AUDIO_FILE")
        .kill_on_drop(true)
        .status()
        .await?;
    anyhow::ensure!(
        status.success(),
        "installed capture-to-note self-test failed; artifacts: {}; configuration: {}",
        work.display(),
        default_config_path()?.display()
    );
    if std::env::var_os("RUSTLE_TEST_ARTIFACTS").is_none() {
        tokio::fs::remove_dir_all(&work).await?;
    }
    Ok(())
}
