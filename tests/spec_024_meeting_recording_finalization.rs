use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustle_core::{AppEvent, DetectionSource, EventBus};

const TEST_AUDIO_FILE_ENV: &str = "RUSTLE_TEST_AUDIO_FILE";

#[tokio::test]
async fn spec_024_meeting_waits_for_final_recording_chunk_before_transcription(
) -> Result<(), String> {
    let sandbox = unique_sandbox("spec-024-final-recording-chunk");
    let config_home = sandbox.join("config");
    let data_home = sandbox.join("data");

    tokio::fs::create_dir_all(config_home.join("rustle"))
        .await
        .map_err(|error| format!("config directory creation failed: {error}"))?;

    let previous_xdg_config_home = std::env::var_os("XDG_CONFIG_HOME");
    let previous_xdg_data_home = std::env::var_os("XDG_DATA_HOME");
    let previous_test_audio = std::env::var_os(TEST_AUDIO_FILE_ENV);

    std::env::set_var("XDG_CONFIG_HOME", &config_home);
    std::env::set_var("XDG_DATA_HOME", &data_home);
    std::env::remove_var(TEST_AUDIO_FILE_ENV);

    let bus = EventBus::new();
    let sender = bus.sender();
    let mut observer = bus.subscribe();

    rustle_transcription::initialise(sender, Some(bus.subscribe()))
        .await
        .expect("transcription should initialise");

    let meeting_id = uuid::Uuid::new_v4();
    bus.publish(AppEvent::MeetingStarted {
        id: meeting_id,
        name: "Recorded Meeting".to_owned(),
        source: DetectionSource::Manual,
    })
    .expect("meeting start should publish");

    let transcript_path = wait_for_transcript_draft(&mut observer, meeting_id).await?;

    bus.publish(AppEvent::RecordingStarted { meeting_id })
        .expect("recording start should publish");
    bus.publish(AppEvent::MeetingEnded { id: meeting_id })
        .expect("meeting end should publish");

    assert_no_transcription_ready(&mut observer, meeting_id).await?;

    let missing_chunk = sandbox.join("missing-final-chunk.wav");
    bus.publish(AppEvent::RecordingChunkReady {
        meeting_id,
        path: missing_chunk.clone(),
    })
    .expect("recording chunk should publish");
    bus.publish(AppEvent::RecordingStopped { meeting_id })
        .expect("recording stop should publish");

    let segments = wait_for_transcription_ready(&mut observer, meeting_id).await?;
    bus.publish(AppEvent::QuitRequested)
        .expect("quit should publish");

    let transcript = tokio::fs::read_to_string(&transcript_path)
        .await
        .map_err(|error| format!("transcript draft read failed: {error}"))?;

    assert_eq!(segments.len(), 1);
    assert!(segments[0]
        .text
        .contains("speech transcription was not available"));
    assert!(segments[0]
        .text
        .contains(&missing_chunk.display().to_string()));
    assert!(transcript.contains("speech transcription was not available"));
    assert!(transcript_path.starts_with(data_home.join("rustle/transcripts")));

    restore_env_var("XDG_CONFIG_HOME", previous_xdg_config_home);
    restore_env_var("XDG_DATA_HOME", previous_xdg_data_home);
    restore_env_var(TEST_AUDIO_FILE_ENV, previous_test_audio);

    let _ = tokio::fs::remove_dir_all(&sandbox).await;

    Ok(())
}

async fn wait_for_transcript_draft(
    observer: &mut rustle_core::EventReceiver,
    meeting_id: uuid::Uuid,
) -> Result<PathBuf, String> {
    wait_for_event(observer, move |event| match event {
        AppEvent::TranscriptDraftReady {
            meeting_id: observed_id,
            path,
        } if *observed_id == meeting_id => Some(path.clone()),
        _ => None,
    })
    .await
}

async fn wait_for_transcription_ready(
    observer: &mut rustle_core::EventReceiver,
    meeting_id: uuid::Uuid,
) -> Result<Vec<rustle_core::TranscriptSegment>, String> {
    wait_for_event(observer, move |event| match event {
        AppEvent::TranscriptionReady {
            meeting_id: observed_id,
            segments,
        } if *observed_id == meeting_id => Some(segments.clone()),
        _ => None,
    })
    .await
}

async fn assert_no_transcription_ready(
    observer: &mut rustle_core::EventReceiver,
    meeting_id: uuid::Uuid,
) -> Result<(), String> {
    let result = tokio::time::timeout(Duration::from_millis(150), async {
        loop {
            match observer.recv().await {
                Ok(AppEvent::TranscriptionReady { meeting_id: id, .. }) if id == meeting_id => {
                    return Err(
                        "TranscriptionReady arrived before final recording chunk".to_owned()
                    );
                }
                Ok(_) => continue,
                Err(error) => return Err(error.to_string()),
            }
        }
    })
    .await;

    match result {
        Ok(Ok(())) => Err("observer ended before timeout".to_owned()),
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(()),
    }
}

async fn wait_for_event<T>(
    observer: &mut rustle_core::EventReceiver,
    mut select: impl FnMut(&AppEvent) -> Option<T>,
) -> Result<T, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = tokio::time::timeout(remaining, observer.recv())
            .await
            .map_err(|_| "timed out waiting for workflow event".to_owned())?
            .map_err(|error| error.to_string())?;

        if let Some(value) = select(&event) {
            return Ok(value);
        }
    }
}

fn unique_sandbox(test_name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("rustle-{test_name}-{unique}"))
}

fn restore_env_var(name: &str, value: Option<std::ffi::OsString>) {
    match value {
        Some(value) => std::env::set_var(name, value),
        None => std::env::remove_var(name),
    }
}
