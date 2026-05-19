//! Audio capture crate for Rustle.

use rustle_core::{tasks::spawn_logged, AppEvent, CoreError, EventReceiver, EventSender};
use tracing::{info, warn};

/// Initialises the audio capture component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    drop(event_tx);

    if let Some(receiver) = event_rx {
        spawn_logged("audio-placeholder-loop", audio_placeholder_loop(receiver));
    }

    Ok(())
}

async fn audio_placeholder_loop(mut event_rx: EventReceiver) {
    info!("audio capture is deferred in the manual notes MVP");

    loop {
        match event_rx.recv().await {
            Ok(AppEvent::MeetingStarted { name, .. }) => {
                info!(meeting = %name, "manual MVP is not recording audio");
            }
            Ok(AppEvent::QuitRequested) => break,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "audio placeholder loop lagged behind event bus");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}
