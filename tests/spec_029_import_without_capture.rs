use rustle_core::{AppEvent, EventBus};
use std::time::Duration;

#[tokio::test]
async fn spec_029_importing_a_transcript_does_not_start_a_meeting() {
    let root = std::env::temp_dir().join(format!("rustle-import-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("123_Planning.txt");
    std::fs::write(&path, "Alice: Approved for Friday.").unwrap();
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    let bus = EventBus::new();
    let mut observer = bus.subscribe();
    rustle_transcription::initialise(bus.sender(), Some(bus.subscribe()))
        .await
        .unwrap();
    bus.publish(AppEvent::SummariseTranscriptRequested { path })
        .unwrap();
    let mut named = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match observer.recv().await.unwrap() {
                AppEvent::MeetingStarted { .. } => panic!("import must not start recording"),
                AppEvent::TranscriptImported { name, .. } => {
                    assert_eq!(name, "Planning");
                    named = true;
                }
                AppEvent::TranscriptionReady { segments, .. } => {
                    assert!(named);
                    assert!(segments[0].text.contains("Friday"));
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    bus.publish(AppEvent::ShutdownComplete).unwrap();
    tokio::task::yield_now().await;
    std::fs::remove_dir_all(root).unwrap();
}
