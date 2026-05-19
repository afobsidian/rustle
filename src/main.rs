//! Rustle application entry point.

use rustle_core::{AppEvent, EventBus, Settings};
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_tracing();

    Settings::load().await?;
    let event_bus = EventBus::new();
    let sender = event_bus.sender();

    rustle_tray::initialise(sender.clone(), Some(event_bus.subscribe())).await?;
    rustle_detection::initialise(sender.clone(), Some(event_bus.subscribe())).await?;
    rustle_audio::initialise(sender.clone(), Some(event_bus.subscribe())).await?;
    rustle_transcription::initialise(sender.clone(), Some(event_bus.subscribe())).await?;
    rustle_ai::initialise(sender.clone(), Some(event_bus.subscribe())).await?;
    rustle_storage::initialise(sender.clone(), Some(event_bus.subscribe())).await?;
    rustle_ui::initialise(sender, Some(event_bus.subscribe())).await?;

    info!("rustle initialised");
    wait_for_shutdown(event_bus.subscribe()).await;
    Ok(())
}

async fn wait_for_shutdown(mut receiver: rustle_core::EventReceiver) {
    loop {
        tokio::select! {
            event = receiver.recv() => match event {
                Ok(AppEvent::QuitRequested) => {
                    info!("quit requested");
                    break;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "shutdown listener lagged behind event bus");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            signal = tokio::signal::ctrl_c() => {
                if let Err(error) = signal {
                    tracing::warn!(%error, "failed to listen for ctrl-c");
                }
                info!("ctrl-c received");
                break;
            }
        }
    }
}

fn install_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
