use rustle_core::{AppEvent, DetectionSource, EventBus, Settings};
use std::time::Duration;

#[tokio::test]
async fn spec_028_short_automatic_meeting_retains_transcript_and_recording() {
    let sandbox = std::env::temp_dir().join(format!("rustle-short-{}", uuid::Uuid::new_v4()));
    std::env::set_var("XDG_CONFIG_HOME", sandbox.join("config"));
    std::env::set_var("XDG_DATA_HOME", sandbox.join("data"));
    std::env::remove_var("RUSTLE_TEST_AUDIO_FILE");
    let mut settings = Settings::default();
    settings.meeting.auto_capture = false;
    settings.save().await.unwrap();
    let bus = EventBus::new();
    let mut observer = bus.subscribe();
    rustle_transcription::initialise(bus.sender(), Some(bus.subscribe()))
        .await
        .unwrap();
    let id = uuid::Uuid::new_v4();
    bus.publish(AppEvent::MeetingStarted {
        id,
        name: "Short call".into(),
        source: DetectionSource::Hyprland,
    })
    .unwrap();
    let path = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let AppEvent::TranscriptDraftReady { path, .. } = observer.recv().await.unwrap() {
                break path;
            }
        }
    })
    .await
    .unwrap();
    std::fs::write(&path, "Alice: The release is approved for Friday.").unwrap();
    let recording = sandbox.join("retained.wav");
    std::fs::write(&recording, b"retained source recording").unwrap();
    bus.publish(AppEvent::MeetingEnded { id }).unwrap();
    bus.publish(AppEvent::RecordingStopped { meeting_id: id })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match observer.recv().await.unwrap() {
                AppEvent::TranscriptionReady { segments, .. } => {
                    assert!(segments[0].text.contains("Friday"));
                    break;
                }
                AppEvent::DocumentDeleted { .. } => {
                    panic!("short meetings must not delete captured material")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(path.is_file());
    assert!(recording.is_file());
    bus.publish(AppEvent::ShutdownComplete).unwrap();
    tokio::task::yield_now().await;
    std::fs::remove_dir_all(sandbox).unwrap();
}
