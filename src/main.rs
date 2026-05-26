//! Rustle application entry point.

mod autostart;
mod diagnostics;
mod notifications;

use rustle_core::{is_hyprland_session, supported_session_label, AppEvent, EventBus, Settings};
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let diagnostics = diagnostics::install();
    info!(
        subsystem = "diagnostics",
        log_path = diagnostics
            .log_path
            .as_ref()
            .map(|path| path.display().to_string()),
        "diagnostics initialised"
    );

    let settings = load_startup_settings().await?;
    if let Err(error) = autostart::reconcile(&settings).await {
        warn!(subsystem = "autostart", %error, "failed to reconcile start-on-login integration");
    }
    warn_if_unsupported_session();

    let event_bus = EventBus::new();
    let sender = event_bus.sender();

    initialise_component(
        "notifications",
        notifications::initialise(Some(event_bus.subscribe())),
    )
    .await?;
    initialise_component(
        "tray",
        rustle_tray::initialise(sender.clone(), Some(event_bus.subscribe())),
    )
    .await?;
    initialise_component(
        "detection",
        rustle_detection::initialise(sender.clone(), Some(event_bus.subscribe())),
    )
    .await?;
    initialise_component(
        "audio",
        rustle_audio::initialise(sender.clone(), Some(event_bus.subscribe())),
    )
    .await?;
    initialise_component(
        "transcription",
        rustle_transcription::initialise(sender.clone(), Some(event_bus.subscribe())),
    )
    .await?;
    initialise_component(
        "ai",
        rustle_ai::initialise(sender.clone(), Some(event_bus.subscribe())),
    )
    .await?;
    initialise_component(
        "storage",
        rustle_storage::initialise(sender.clone(), Some(event_bus.subscribe())),
    )
    .await?;
    initialise_component(
        "ui",
        rustle_ui::initialise(sender, Some(event_bus.subscribe())),
    )
    .await?;

    info!("rustle initialised");
    wait_for_shutdown(event_bus.subscribe()).await;
    Ok(())
}

async fn load_startup_settings() -> anyhow::Result<Settings> {
    match Settings::load().await {
        Ok(settings) => {
            info!(subsystem = "settings", "startup settings loaded");
            Ok(settings)
        }
        Err(error) => {
            error!(subsystem = "settings", %error, "failed to load startup settings");
            Err(error.into())
        }
    }
}

async fn initialise_component<T>(
    component: &'static str,
    future: impl std::future::Future<Output = Result<(), T>>,
) -> Result<(), T>
where
    T: std::fmt::Display,
{
    info!(subsystem = component, "initialising subsystem");
    match future.await {
        Ok(()) => {
            info!(subsystem = component, "subsystem initialised");
            Ok(())
        }
        Err(error) => {
            error!(subsystem = component, %error, "subsystem initialisation failed");
            Err(error)
        }
    }
}

fn warn_if_unsupported_session() {
    if !is_hyprland_session() {
        warn!(
            expected_session = supported_session_label(),
            "Rustle is only supported on Hyprland; continuing in an unsupported session"
        );
    }
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
