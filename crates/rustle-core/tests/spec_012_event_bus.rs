use rustle_core::{AppEvent, EventBus};

#[tokio::test]
async fn spec_012_event_bus_delivers_shutdown_events_without_crashing() {
    let bus = EventBus::new();
    let mut receiver = bus.subscribe();

    let delivered = bus
        .publish(AppEvent::QuitRequested)
        .expect("event should publish to an active receiver");

    assert_eq!(delivered, 1);
    assert!(matches!(
        receiver.recv().await.expect("event should be received"),
        AppEvent::QuitRequested
    ));
}

#[test]
fn spec_012_event_bus_reports_when_no_receivers_are_available() {
    let bus = EventBus::new();

    let error = bus
        .publish(AppEvent::QuitRequested)
        .expect_err("publishing without receivers should be reported");

    assert!(matches!(
        *error,
        tokio::sync::broadcast::error::SendError(_)
    ));
}
