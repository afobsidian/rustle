use std::os::unix::fs::PermissionsExt;

use rustle_core::{
    AiProvider, LoginMethod, RecordingDetectionMethod, Settings, TranscriptionMethod,
};
use uuid::Uuid;

fn temp_config_path(test_name: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join("rustle-spec-009")
        .join(test_name)
        .join(format!("{}.toml", Uuid::new_v4()))
}

#[test]
fn spec_009_defaults_match_persistent_settings_schema() {
    let settings = Settings::default();

    assert!(!settings.general.start_on_login);
    assert_eq!(settings.general.start_on_login_method, LoginMethod::Xdg);
    assert!(settings.meeting.auto_detect);
    assert!(settings.meeting.auto_capture);
    assert_eq!(
        settings.meeting.detection_method,
        RecordingDetectionMethod::Hyprland
    );
    assert_eq!(settings.audio.input_device, "default");
    assert!(!settings.audio.capture_loopback);
    assert_eq!(settings.audio.max_recording_size_mb, 2048);
    assert_eq!(settings.audio.chunk_duration_minutes, 10);
    assert_eq!(settings.transcription.method, TranscriptionMethod::Local);
    assert_eq!(
        settings.transcription.model_path,
        "~/.local/share/rustle/models/ggml-base.en.bin"
    );
    assert_eq!(settings.transcription.openai_api_key, "");
    assert_eq!(settings.ai.provider, AiProvider::Openai);
    assert_eq!(settings.ai.model, "gpt-4o");
    assert_eq!(settings.ai.api_key, "");
    assert_eq!(settings.ai.ollama_url, "http://localhost:11434");
    assert!(settings
        .ai
        .system_prompt
        .contains("meeting notes assistant"));
    assert_eq!(settings.storage.notes_dir, "~/.local/share/rustle/notes");
    assert_eq!(settings.storage.db_path, "~/.local/share/rustle/rustle.db");
}

#[tokio::test]
async fn spec_009_missing_config_loads_sensible_defaults() {
    let path = temp_config_path("missing-config");

    let settings = Settings::load_from_path(&path)
        .await
        .expect("missing settings should load defaults");

    assert_eq!(settings, Settings::default());
}

#[tokio::test]
async fn spec_009_config_round_trips_as_toml_with_owner_only_permissions() {
    let path = temp_config_path("round-trip");
    let mut settings = Settings::default();
    settings.general.start_on_login = true;
    settings.audio.input_device = "pipewire.monitor".to_owned();
    settings.ai.provider = AiProvider::Ollama;
    settings.ai.model = "llama3.1".to_owned();

    settings
        .save_to_path(&path)
        .await
        .expect("settings should save");
    let loaded = Settings::load_from_path(&path)
        .await
        .expect("settings should reload");
    let mode = std::fs::metadata(&path)
        .expect("settings file should exist")
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(loaded, settings);
    assert_eq!(mode, 0o600);

    std::fs::remove_file(&path).expect("settings file should be removed");
    let parent = path.parent().expect("settings path should have parent");
    std::fs::remove_dir_all(parent).expect("settings temp dir should be removed");
}

#[tokio::test]
async fn spec_009_invalid_values_fall_back_to_defaults() {
    let path = temp_config_path("invalid-values");
    let parent = path.parent().expect("settings path should have parent");
    std::fs::create_dir_all(parent).expect("settings temp dir should be created");
    std::fs::write(
        &path,
        "[meeting]
detection_method = 'not-supported'

[audio]
input_device = ''
max_recording_size_mb = 0
chunk_duration_minutes = 0

[storage]
notes_dir = ''
db_path = ''
",
    )
    .expect("invalid settings should be written");

    let settings = Settings::load_from_path(&path)
        .await
        .expect("invalid settings should load with defaults");

    assert_eq!(settings.meeting, Default::default());
    assert_eq!(settings.audio, Default::default());
    assert_eq!(settings.storage, Default::default());

    std::fs::remove_file(&path).expect("settings file should be removed");
    std::fs::remove_dir_all(parent).expect("settings temp dir should be removed");
}
