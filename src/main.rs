//! Rustle application entry point.

use rustle_core::{EventBus, Settings};
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_tracing();

    let settings = Settings::load().await?;
    let event_bus = EventBus::new();
    let sender = event_bus.sender();

    rustle_tray::initialise(sender.clone(), Some(event_bus.subscribe())).await?;
    rustle_detection::initialise(sender.clone(), Some(event_bus.subscribe())).await?;
    rustle_audio::initialise(sender.clone(), Some(event_bus.subscribe())).await?;
    rustle_transcription::initialise(sender.clone(), Some(event_bus.subscribe())).await?;
    rustle_ai::initialise(sender.clone(), Some(event_bus.subscribe())).await?;
    rustle_storage::initialise(sender.clone(), Some(event_bus.subscribe())).await?;
    rustle_ui::initialise(sender, Some(event_bus.subscribe())).await?;

    info!(?settings, "rustle initialised");
    Ok(())
}

fn install_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
