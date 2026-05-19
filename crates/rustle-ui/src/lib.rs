//! User interface crate for Rustle.

use std::path::PathBuf;
use std::process::Stdio;

use rustle_core::{
    resolve_notes_dir, tasks::spawn_logged, AppEvent, CoreError, EventReceiver, EventSender,
    Settings,
};
use tokio::process::Command;
use tracing::{info, warn};

/// Initialises the user interface component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    drop(event_tx);

    if let Some(receiver) = event_rx {
        spawn_logged("notes-open-loop", notes_open_loop(receiver));
    }

    Ok(())
}

async fn notes_open_loop(mut event_rx: EventReceiver) {
    let mut latest_note: Option<PathBuf> = None;

    loop {
        match event_rx.recv().await {
            Ok(AppEvent::NoteSaved { path, .. }) => {
                latest_note = Some(path);
            }
            Ok(AppEvent::OpenNotesRequested) => {
                let path = match latest_note.clone() {
                    Some(path) => path,
                    None => configured_notes_dir().await,
                };
                open_path(&path).await;
            }
            Ok(AppEvent::QuitRequested) => break,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "notes UI loop lagged behind event bus");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn configured_notes_dir() -> PathBuf {
    let settings = match Settings::load().await {
        Ok(settings) => settings,
        Err(error) => {
            warn!(%error, "failed to load settings for opening notes; using defaults");
            Settings::default().validated()
        }
    };

    resolve_notes_dir(&settings).unwrap_or_else(|_| PathBuf::from("."))
}

async fn open_path(path: &PathBuf) {
    info!(path = %path.display(), "opening notes path");
    match Command::new("xdg-open")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_child) => {}
        Err(error) => warn!(%error, path = %path.display(), "failed to open notes path"),
    }
}
