use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustle_core::{AppEvent, DetectionSource, EventBus, NotificationUrgency};

const TRANSCRIPT_FIXTURE: &str = include_str!("fixtures/manual_workflow_transcript.txt");
const TEST_AUDIO_FILE_ENV: &str = "RUSTLE_TEST_AUDIO_FILE";

#[tokio::test]
async fn spec_021_manual_workflow_persists_transcript_and_notes() {
    let sandbox = unique_sandbox("spec-021-manual-workflow");
    let config_home = sandbox.join("config");
    let data_home = sandbox.join("data");
    let config_path = config_home.join("rustle/config.toml");
    let transcript_fixture = sandbox.join("fixtures/manual_workflow_transcript.txt");

    tokio::fs::create_dir_all(transcript_fixture.parent().unwrap())
        .await
        .expect("fixture directory should exist");
    tokio::fs::create_dir_all(config_path.parent().unwrap())
        .await
        .expect("config directory should exist");
    tokio::fs::write(&transcript_fixture, TRANSCRIPT_FIXTURE)
        .await
        .expect("transcript fixture should be written");
    tokio::fs::write(&config_path, "[ai]\nprovider = 'openai'\n")
        .await
        .expect("config should be written");

    let previous_xdg_config_home = std::env::var_os("XDG_CONFIG_HOME");
    let previous_xdg_data_home = std::env::var_os("XDG_DATA_HOME");
    let previous_test_audio = std::env::var_os(TEST_AUDIO_FILE_ENV);

    std::env::set_var("XDG_CONFIG_HOME", &config_home);
    std::env::set_var("XDG_DATA_HOME", &data_home);
    std::env::set_var(TEST_AUDIO_FILE_ENV, &transcript_fixture);

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

    let meeting_name = "Manual Workflow Release Contract";
    let meeting_id = uuid::Uuid::new_v4();
    bus.publish(AppEvent::MeetingStarted {
        id: meeting_id,
        name: meeting_name.to_owned(),
        source: DetectionSource::Manual,
    })
    .expect("meeting start should publish");

    let transcript_path = wait_for_transcript_draft(&mut observer, meeting_id)
        .await
        .expect("transcript draft should be published");

    bus.publish(AppEvent::MeetingEnded { id: meeting_id })
        .expect("meeting end should publish");

    let notification = wait_for_fallback_notification(&mut observer)
        .await
        .expect("fallback notification should be published");
    let note_path = wait_for_note_saved(&mut observer, meeting_id)
        .await
        .expect("note should be saved");

    bus.publish(AppEvent::QuitRequested)
        .expect("quit should publish");

    let transcript = tokio::fs::read_to_string(&transcript_path)
        .await
        .expect("transcript draft should exist");
    let notes = tokio::fs::read_to_string(&note_path)
        .await
        .expect("note should exist");

    assert!(transcript_path.starts_with(data_home.join("rustle/transcripts")));
    assert!(note_path.starts_with(data_home.join("rustle/notes")));
    assert!(transcript.contains("Alice: We approved the Fedora release candidate."));
    assert!(transcript.contains("Morgan: I'll publish the smoke matrix tomorrow."));
    assert!(notes.contains("# Meeting Notes"));
    assert!(notes.contains("## Warning"));
    assert!(notes.contains("not supported in Rustle v0.1"));
    assert!(notes.contains("Transcript draft remains available separately"));
    assert!(notification.contains("Manual Workflow Release Contract"));
    assert!(notification.contains("fallback notes"));

    restore_env_var("XDG_CONFIG_HOME", previous_xdg_config_home);
    restore_env_var("XDG_DATA_HOME", previous_xdg_data_home);
    restore_env_var(TEST_AUDIO_FILE_ENV, previous_test_audio);

    let _ = tokio::fs::remove_dir_all(&sandbox).await;
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

async fn wait_for_note_saved(
    observer: &mut rustle_core::EventReceiver,
    meeting_id: uuid::Uuid,
) -> Result<PathBuf, String> {
    wait_for_event(observer, move |event| match event {
        AppEvent::NoteSaved {
            meeting_id: observed_id,
            path,
        } if *observed_id == meeting_id => Some(path.clone()),
        _ => None,
    })
    .await
}

async fn wait_for_fallback_notification(
    observer: &mut rustle_core::EventReceiver,
) -> Result<String, String> {
    wait_for_event(observer, |event| match event {
        AppEvent::NotificationRequested {
            title,
            body,
            urgency,
        } if title == "Notes fallback saved" && *urgency == NotificationUrgency::Critical => {
            Some(body.clone())
        }
        _ => None,
    })
    .await
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
