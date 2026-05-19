//! Meeting detection crate for Rustle.

use rustle_core::{tasks::spawn_logged, AppEvent, CoreError, EventReceiver, EventSender, Settings};
use tracing::{info, warn};

/// Initialises the meeting detection component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    drop(event_tx);

    if let Some(receiver) = event_rx {
        spawn_logged(
            "detection-placeholder-loop",
            detection_placeholder_loop(receiver),
        );
    }

    Ok(())
}

async fn detection_placeholder_loop(mut event_rx: EventReceiver) {
    let settings = match Settings::load().await {
        Ok(settings) => settings,
        Err(error) => {
            warn!(%error, "failed to load settings for meeting detection; using defaults");
            Settings::default().validated()
        }
    };
    info!(
        auto_detect = settings.meeting.auto_detect,
        method = ?settings.meeting.detection_method,
        "automatic Teams detection is deferred in the manual notes MVP"
    );

    loop {
        match event_rx.recv().await {
            Ok(AppEvent::QuitRequested) => break,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(
                    skipped,
                    "detection placeholder loop lagged behind event bus"
                );
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}
