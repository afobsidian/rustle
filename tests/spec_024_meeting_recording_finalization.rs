use rustle_core::{AppEvent, DetectionSource, EventBus};
use std::time::Duration;

#[tokio::test]
async fn spec_024_final_chunk_failure_is_reported_and_draft_retained() {
    let sandbox =
        std::env::temp_dir().join(format!("rustle-finalisation-{}", uuid::Uuid::new_v4()));
    std::env::set_var("XDG_CONFIG_HOME", sandbox.join("config"));
    std::env::set_var("XDG_DATA_HOME", sandbox.join("data"));
    std::env::remove_var("RUSTLE_TEST_AUDIO_FILE");
    let bus = EventBus::new();
    let mut observer = bus.subscribe();
    rustle_transcription::initialise(bus.sender(), Some(bus.subscribe()))
        .await
        .unwrap();
    let id = uuid::Uuid::new_v4();
    bus.publish(AppEvent::MeetingStarted {
        id,
        name: "Final chunk".into(),
        source: DetectionSource::Manual,
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
    // End before RecordingStarted: capture intent must still hold finalisation open.
    bus.publish(AppEvent::MeetingEnded { id }).unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(50), async {
        loop {
            if matches!(
                observer.recv().await.unwrap(),
                AppEvent::MeetingFinished { .. }
            ) {
                break;
            }
        }
    })
    .await
    .is_err());
    bus.publish(AppEvent::RecordingChunkReady {
        meeting_id: id,
        path: sandbox.join("missing.wav"),
    })
    .unwrap();
    bus.publish(AppEvent::RecordingStopped { meeting_id: id })
        .unwrap();
    let mut failed = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match observer.recv().await.unwrap() {
                AppEvent::WorkflowFailed { stage, .. } => {
                    assert_eq!(stage, "transcription");
                    failed = true;
                }
                AppEvent::TranscriptionReady { .. } => {
                    panic!("a missing recording must not become fake speech")
                }
                AppEvent::MeetingFinished { .. } => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(failed);
    assert!(path.is_file());
    bus.publish(AppEvent::ShutdownComplete).unwrap();
    tokio::task::yield_now().await;
    std::fs::remove_dir_all(sandbox).unwrap();
}
