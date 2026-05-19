//! Storage crate for Rustle.

use std::collections::HashMap;
use std::path::PathBuf;

use rustle_core::{
    resolve_notes_dir, safe_filename, tasks::spawn_logged, AppEvent, CoreError, EventReceiver,
    EventSender, MeetingNotes, Settings,
};
use tokio::fs;
use tracing::{info, warn};

/// Initialises the storage component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    if let Some(receiver) = event_rx {
        spawn_logged("storage-loop", storage_loop(event_tx, receiver));
    }

    Ok(())
}

async fn storage_loop(event_tx: EventSender, mut event_rx: EventReceiver) {
    let mut meeting_names = HashMap::new();

    loop {
        match event_rx.recv().await {
            Ok(AppEvent::MeetingStarted { id, name, .. }) => {
                meeting_names.insert(id, name);
            }
            Ok(AppEvent::SummarisationReady { meeting_id, notes }) => {
                let meeting_name = meeting_names
                    .remove(&meeting_id)
                    .unwrap_or_else(|| "Meeting".to_owned());
                match save_notes(&meeting_name, &notes).await {
                    Ok(path) => {
                        info!(path = %path.display(), "meeting notes written");
                        publish(&event_tx, AppEvent::NoteSaved { meeting_id, path });
                    }
                    Err(error) => warn!(%error, meeting_id = %meeting_id, "failed to save notes"),
                }
            }
            Ok(AppEvent::QuitRequested) => break,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "storage loop lagged behind event bus");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn save_notes(meeting_name: &str, notes: &MeetingNotes) -> std::io::Result<PathBuf> {
    let settings = match Settings::load().await {
        Ok(settings) => settings,
        Err(error) => {
            warn!(%error, "failed to load settings for note storage; using defaults");
            Settings::default().validated()
        }
    };
    let directory = resolve_notes_dir(&settings)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
    fs::create_dir_all(&directory).await?;

    let file_name = format!(
        "{}_{}.md",
        unix_timestamp_seconds(),
        safe_filename(meeting_name)
    );
    let path = directory.join(file_name);
    fs::write(&path, render_markdown(notes)).await?;
    Ok(path)
}

fn render_markdown(notes: &MeetingNotes) -> String {
    if notes.markdown.trim().is_empty() {
        return format!("# Meeting Notes\n\n## Summary\n\n{}\n", notes.summary);
    }

    notes.markdown.clone()
}

fn publish(event_tx: &EventSender, event: AppEvent) {
    if let Err(error) = event_tx.send(event) {
        warn!(%error, "failed to publish event");
    }
}

fn unix_timestamp_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_markdown_uses_existing_markdown() {
        let notes = MeetingNotes {
            summary: "summary".to_owned(),
            key_decisions: Vec::new(),
            action_items: Vec::new(),
            attendees: Vec::new(),
            markdown: "# Existing".to_owned(),
        };

        assert_eq!(render_markdown(&notes), "# Existing");
    }
}
