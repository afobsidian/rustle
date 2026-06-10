use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustle_core::{AppEvent, DetectionSource, EventBus, StoredDocumentKind};

#[tokio::test]
async fn spec_028_short_hyprland_detection_discards_false_positive_artifacts() -> Result<(), String>
{
    let sandbox = unique_sandbox("spec-028-short-hyprland-false-positive");
    let config_home = sandbox.join("config");
    let data_home = sandbox.join("data");

    tokio::fs::create_dir_all(config_home.join("rustle"))
        .await
        .map_err(|error| format!("config directory creation failed: {error}"))?;

    let previous_xdg_config_home = std::env::var_os("XDG_CONFIG_HOME");
    let previous_xdg_data_home = std::env::var_os("XDG_DATA_HOME");

    std::env::set_var("XDG_CONFIG_HOME", &config_home);
    std::env::set_var("XDG_DATA_HOME", &data_home);

    let bus = EventBus::new();
    let sender = bus.sender();
    let mut observer = bus.subscribe();

    rustle_transcription::initialise(sender, Some(bus.subscribe()))
        .await
        .expect("transcription should initialise");

    let meeting_id = uuid::Uuid::new_v4();
    bus.publish(AppEvent::MeetingStarted {
        id: meeting_id,
        name: "(1) Calendar | False Positive".to_owned(),
        source: DetectionSource::Hyprland,
    })
    .expect("meeting start should publish");

    let transcript_path = wait_for_transcript_draft(&mut observer, meeting_id).await?;
    assert!(
        tokio::fs::try_exists(&transcript_path)
            .await
            .map_err(|error| format!("draft existence check failed: {error}"))?,
        "short Hyprland detection should still create draft before finalisation"
    );

    bus.publish(AppEvent::MeetingEnded { id: meeting_id })
        .expect("meeting end should publish");

    let deleted_path = wait_for_document_deleted(&mut observer, &transcript_path).await?;
    assert_eq!(deleted_path, transcript_path);

    assert!(
        !tokio::fs::try_exists(&transcript_path)
            .await
            .map_err(|error| format!("draft deletion check failed: {error}"))?,
        "short Hyprland detection draft should be removed"
    );
    assert_no_transcription_ready(&mut observer, meeting_id).await?;

    bus.publish(AppEvent::QuitRequested)
        .expect("quit should publish");

    restore_env_var("XDG_CONFIG_HOME", previous_xdg_config_home);
    restore_env_var("XDG_DATA_HOME", previous_xdg_data_home);

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

async fn wait_for_document_deleted(
    observer: &mut rustle_core::EventReceiver,
    expected_path: &Path,
) -> Result<PathBuf, String> {
    let expected_path = expected_path.to_path_buf();
    wait_for_event(observer, move |event| match event {
        AppEvent::DocumentDeleted { path, kind }
            if *kind == StoredDocumentKind::Transcript && *path == expected_path =>
        {
            Some(path.clone())
        }
        _ => None,
    })
    .await
}

async fn assert_no_transcription_ready(
    observer: &mut rustle_core::EventReceiver,
    meeting_id: uuid::Uuid,
) -> Result<(), String> {
    let result = tokio::time::timeout(Duration::from_millis(250), async {
        loop {
            match observer.recv().await {
                Ok(AppEvent::TranscriptionReady { meeting_id: id, .. }) if id == meeting_id => {
                    return Err(
                        "TranscriptionReady should not be published for short Hyprland false positives"
                            .to_owned(),
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
