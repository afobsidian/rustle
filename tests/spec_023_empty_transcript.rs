use std::time::Duration;

use rustle_core::{AppEvent, DetectionSource, EventBus};

const TEST_AUDIO_FILE_ENV: &str = "RUSTLE_TEST_AUDIO_FILE";

#[tokio::test]
async fn spec_023_empty_transcript_does_not_trigger_summarisation() -> Result<(), String> {
    let sandbox = unique_sandbox("spec-023-empty-transcript");
    let config_home = sandbox.join("config");
    let data_home = sandbox.join("data");
    let config_path = config_home.join("rustle/config.toml");

    tokio::fs::create_dir_all(config_path.parent().unwrap())
        .await
        .map_err(|e| format!("config directory creation failed: {e}"))?;

    tokio::fs::write(&config_path, "[ai]\nprovider = 'openai'\n")
        .await
        .map_err(|e| format!("config write failed: {e}"))?;

    let previous_xdg_config_home = std::env::var_os("XDG_CONFIG_HOME");
    let previous_xdg_data_home = std::env::var_os("XDG_DATA_HOME");
    let previous_test_audio = std::env::var_os(TEST_AUDIO_FILE_ENV);

    std::env::set_var("XDG_CONFIG_HOME", &config_home);
    std::env::set_var("XDG_DATA_HOME", &data_home);
    // Ensure no test audio fixture is configured — we want an empty transcript.
    std::env::remove_var(TEST_AUDIO_FILE_ENV);

    let bus = EventBus::new();
    let sender = bus.sender();
    let mut observer = bus.subscribe();

    rustle_transcription::initialise(sender.clone(), Some(bus.subscribe()))
        .await
        .expect("transcription should initialise");
    rustle_ai::initialise(sender.clone(), Some(bus.subscribe()))
        .await
        .expect("ai should initialise");
    rustle_storage::initialise(sender.clone(), Some(bus.subscribe()))
        .await
        .expect("storage should initialise");

    let meeting_id = uuid::Uuid::new_v4();
    bus.publish(AppEvent::MeetingStarted {
        id: meeting_id,
        name: "Empty Meeting".to_owned(),
        source: DetectionSource::Manual,
    })
    .expect("meeting start should publish");

    // Wait for transcript draft
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(remaining, observer.recv())
            .await
            .map_err(|_| "timed out waiting for transcript draft".to_owned())?
            .map_err(|e| e.to_string())?;

        if matches!(event, AppEvent::TranscriptDraftReady { meeting_id: id, .. } if id == meeting_id) {
            break;
        }
    }

    // End the meeting without any recording chunks
    bus.publish(AppEvent::MeetingEnded { id: meeting_id })
        .expect("meeting end should publish");

    // Wait a short time to ensure no TranscriptionReady is published
    let no_transcription = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match observer.recv().await {
                Ok(AppEvent::TranscriptionReady { meeting_id: id, .. }) if id == meeting_id => {
                    return Err("TranscriptionReady should NOT be published for empty transcript".to_owned());
                }
                Ok(AppEvent::QuitRequested) => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        Ok(())
    }).await;

    // The timeout should expire (meaning no TranscriptionReady was published)
    // OR we should get the timeout error
    match no_transcription {
        Ok(Ok(())) => {
            // Restore env before panicking
            restore_env_var("XDG_CONFIG_HOME", previous_xdg_config_home);
            restore_env_var("XDG_DATA_HOME", previous_xdg_data_home);
            restore_env_var(TEST_AUDIO_FILE_ENV, previous_test_audio);
            let _ = tokio::fs::remove_dir_all(&sandbox).await;
            panic!("observer closed unexpectedly");
        }
        Ok(Err(msg)) => {
            restore_env_var("XDG_CONFIG_HOME", previous_xdg_config_home);
            restore_env_var("XDG_DATA_HOME", previous_xdg_data_home);
            restore_env_var(TEST_AUDIO_FILE_ENV, previous_test_audio);
            let _ = tokio::fs::remove_dir_all(&sandbox).await;
            panic!("{}", msg);
        }
        Err(_) => {
            // Timeout expired - this is the expected behavior
            // No TranscriptionReady was published
        }
    }

    bus.publish(AppEvent::QuitRequested).expect("quit should publish");

    restore_env_var("XDG_CONFIG_HOME", previous_xdg_config_home);
    restore_env_var("XDG_DATA_HOME", previous_xdg_data_home);
    restore_env_var(TEST_AUDIO_FILE_ENV, previous_test_audio);

    let _ = tokio::fs::remove_dir_all(&sandbox).await;

    Ok(())
}

fn unique_sandbox(test_name: &str) -> std::path::PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("rustle-test-{test_name}-{ts}"));
    std::fs::create_dir_all(&dir).expect("sandbox directory should be created");
    dir
}

fn restore_env_var(name: &str, value: Option<std::ffi::OsString>) {
    match value {
        Some(val) => std::env::set_var(name, val),
        None => std::env::remove_var(name),
    }
}
