//! System tray crate for Rustle.

use rustle_core::{CoreError, EventReceiver, EventSender};

/// Initialises the system tray component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    drop(event_tx);
    drop(event_rx);
    Ok(())
}
