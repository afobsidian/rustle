//! User interface crate for Rustle.

use std::path::PathBuf;

use rustle_core::{
    default_config_path, resolve_notes_dir, tasks::spawn_logged, AppEvent, CoreError,
    EventReceiver, EventSender, Settings,
};
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
    let mut latest_transcript: Option<PathBuf> = None;

    loop {
        match event_rx.recv().await {
            Ok(AppEvent::NoteSaved { path, .. }) => {
                latest_note = Some(path);
            }
            Ok(AppEvent::TranscriptDraftReady { path, .. }) => {
                latest_transcript = Some(path);
            }
            Ok(AppEvent::OpenNotesRequested) => {
                let path = match latest_note.clone() {
                    Some(path) => path,
                    None => configured_notes_dir().await,
                };
                open_path(&path, "notes").await;
            }
            Ok(AppEvent::OpenTranscriptRequested) => {
                if let Some(path) = latest_transcript.clone() {
                    open_path(&path, "transcript draft").await;
                } else {
                    warn!("no transcript draft is available to open");
                }
            }
            Ok(AppEvent::OpenSettingsRequested) => {
                if let Some(path) = ensure_settings_file().await {
                    open_path(&path, "settings file").await;
                }
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

async fn ensure_settings_file() -> Option<PathBuf> {
    let path = match default_config_path() {
        Ok(path) => path,
        Err(error) => {
            warn!(%error, "failed to resolve settings path");
            return None;
        }
    };

    ensure_settings_file_at(path).await
}

async fn ensure_settings_file_at(path: PathBuf) -> Option<PathBuf> {
    match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => Some(path),
        Ok(_) => {
            warn!(
                path = %path.display(),
                "settings path exists but is not a regular file"
            );
            None
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match Settings::default().save_to_path(&path).await {
                Ok(()) => Some(path),
                Err(error) => {
                    warn!(%error, path = %path.display(), "failed to create settings file");
                    None
                }
            }
        }
        Err(error) => {
            warn!(%error, path = %path.display(), "failed to inspect settings file");
            None
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

async fn open_path(path: &PathBuf, purpose: &'static str) {
    info!(path = %path.display(), purpose, "path");
}

#[cfg(test)]
mod tests {
    use super::ensure_settings_file_at;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_path(prefix: &str) -> std::path::PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("{prefix}-{timestamp}"))
    }

    #[tokio::test]
    async fn creates_missing_settings_file() {
        let temp_root = unique_temp_path("rustle-ui-test");
        let path = temp_root.join("config.toml");

        let created = ensure_settings_file_at(path.clone()).await;

        assert_eq!(created, Some(path.clone()));
        assert!(tokio::fs::metadata(&path).await.unwrap().is_file());
        let _ = tokio::fs::remove_dir_all(&temp_root).await;
    }

    #[tokio::test]
    async fn rejects_settings_path_that_is_not_a_file() {
        let temp_root = unique_temp_path("rustle-ui-test");
        tokio::fs::create_dir_all(&temp_root).await.unwrap();

        let resolved = ensure_settings_file_at(temp_root.clone()).await;

        assert_eq!(resolved, None);
        let _ = tokio::fs::remove_dir_all(&temp_root).await;
    }
}
