//! Storage crate for Rustle.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rustle_core::{
    is_safe_child, resolve_notes_dir, resolve_transcripts_dir, safe_filename, tasks::spawn_logged,
    AppEvent, CoreError, EventReceiver, EventSender, MeetingNotes, Settings, StoredDocument,
    StoredDocumentKind,
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
    let mut settings = load_settings("storage startup").await;

    loop {
        match event_rx.recv().await {
            Ok(AppEvent::MeetingStarted { id, name, .. }) => {
                meeting_names.insert(id, name);
            }
            Ok(AppEvent::SummarisationReady { meeting_id, notes }) => {
                let meeting_name = meeting_names
                    .remove(&meeting_id)
                    .unwrap_or_else(|| "Meeting".to_owned());
                match save_notes(&settings, &meeting_name, &notes).await {
                    Ok(path) => {
                        info!(path = %path.display(), "meeting notes written");
                        publish(&event_tx, AppEvent::NoteSaved { meeting_id, path });
                    }
                    Err(error) => warn!(%error, meeting_id = %meeting_id, "failed to save notes"),
                }
            }
            Ok(AppEvent::DeleteDocumentRequested { path, kind }) => {
                match delete_document(&settings, kind, &path).await {
                    Ok(()) => {
                        info!(path = %path.display(), ?kind, "document deleted");
                        publish(&event_tx, AppEvent::DocumentDeleted { path, kind });
                    }
                    Err(error) => {
                        warn!(%error, path = %path.display(), ?kind, "failed to delete document")
                    }
                }
            }
            Ok(AppEvent::SettingsChanged(updated_settings)) => {
                settings = updated_settings.validated();
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

/// Lists saved notes in newest-first order.
pub async fn list_notes(settings: &Settings) -> std::io::Result<Vec<StoredDocument>> {
    let directory = resolve_notes_dir(settings).map_err(io_error_from_core)?;
    list_documents(directory, StoredDocumentKind::Note, "md").await
}

/// Lists saved transcripts in newest-first order.
pub async fn list_transcripts() -> std::io::Result<Vec<StoredDocument>> {
    let directory = resolve_transcripts_dir().map_err(io_error_from_core)?;
    list_documents(directory, StoredDocumentKind::Transcript, "txt").await
}

async fn save_notes(
    settings: &Settings,
    meeting_name: &str,
    notes: &MeetingNotes,
) -> std::io::Result<PathBuf> {
    let directory = resolve_notes_dir(settings).map_err(io_error_from_core)?;
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

async fn list_documents(
    directory: PathBuf,
    kind: StoredDocumentKind,
    extension: &str,
) -> std::io::Result<Vec<StoredDocument>> {
    let mut entries = match fs::read_dir(&directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };

    let mut documents = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let metadata = entry.metadata().await?;
        if !metadata.is_file() {
            continue;
        }

        if path.extension().and_then(|value| value.to_str()) != Some(extension) {
            continue;
        }

        if let Some(document) = parse_document(path, kind) {
            documents.push(document);
        }
    }

    documents.sort_by(|left, right| {
        right
            .timestamp_seconds
            .cmp(&left.timestamp_seconds)
            .then_with(|| left.title.cmp(&right.title))
    });
    Ok(documents)
}

async fn delete_document(
    settings: &Settings,
    kind: StoredDocumentKind,
    path: &Path,
) -> std::io::Result<()> {
    let base_dir = document_base_dir(settings, kind).map_err(io_error_from_core)?;
    ensure_safe_document_path(&base_dir, path).await?;
    fs::remove_file(path).await
}

fn document_base_dir(settings: &Settings, kind: StoredDocumentKind) -> Result<PathBuf, CoreError> {
    match kind {
        StoredDocumentKind::Note => resolve_notes_dir(settings),
        StoredDocumentKind::Transcript => resolve_transcripts_dir(),
    }
}

async fn ensure_safe_document_path(base_dir: &Path, candidate: &Path) -> std::io::Result<()> {
    let canonical_base = fs::canonicalize(base_dir).await?;
    let canonical_candidate = fs::canonicalize(candidate).await?;
    if is_safe_child(&canonical_base, &canonical_candidate) {
        return Ok(());
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!(
            "refusing to access {} outside {}",
            canonical_candidate.display(),
            canonical_base.display()
        ),
    ))
}

fn parse_document(path: PathBuf, kind: StoredDocumentKind) -> Option<StoredDocument> {
    let stem = path.file_stem()?.to_str()?;
    let (timestamp, title_fragment) = match stem.split_once('_') {
        Some((timestamp, title_fragment)) => (
            timestamp.parse::<u64>().unwrap_or(0),
            humanise_title(title_fragment),
        ),
        None => (0, humanise_title(stem)),
    };

    Some(StoredDocument {
        kind,
        path,
        title: title_fragment,
        timestamp_seconds: timestamp,
    })
}

fn humanise_title(value: &str) -> String {
    let collapsed = value
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.is_empty() {
        "Meeting".to_owned()
    } else {
        collapsed
    }
}

fn io_error_from_core(error: CoreError) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::NotFound, error)
}

async fn load_settings(purpose: &'static str) -> Settings {
    match Settings::load().await {
        Ok(settings) => settings.validated(),
        Err(error) => {
            warn!(%error, purpose, "failed to load settings; using defaults");
            Settings::default().validated()
        }
    }
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
    use std::time::{SystemTime, UNIX_EPOCH};

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

    #[test]
    fn parses_document_title_and_timestamp_from_filename() {
        let document = parse_document(
            PathBuf::from("/tmp/1779283974_team_sync.md"),
            StoredDocumentKind::Note,
        )
        .expect("document should parse");

        assert_eq!(document.timestamp_seconds, 1_779_283_974);
        assert_eq!(document.title, "team sync");
        assert_eq!(document.kind, StoredDocumentKind::Note);
    }

    #[tokio::test]
    async fn spec_018_save_notes_returns_write_errors() {
        let sandbox = unique_sandbox("blocked-notes-dir");
        let blocked_path = sandbox.join("notes-file");
        tokio::fs::create_dir_all(&sandbox)
            .await
            .expect("sandbox should be created");
        tokio::fs::write(&blocked_path, "not a directory")
            .await
            .expect("blocking file should exist");

        let settings = Settings {
            storage: rustle_core::StorageSettings {
                notes_dir: blocked_path.display().to_string(),
                ..Settings::default().storage
            },
            ..Settings::default()
        };
        let notes = MeetingNotes {
            summary: "summary".to_owned(),
            key_decisions: Vec::new(),
            action_items: Vec::new(),
            attendees: Vec::new(),
            markdown: "# Existing".to_owned(),
        };

        let error = save_notes(&settings, "Planning Sync", &notes)
            .await
            .expect_err("blocking file should surface a write error");

        assert!(!error.to_string().is_empty());
        assert!(tokio::fs::try_exists(&blocked_path).await.unwrap_or(false));

        let _ = tokio::fs::remove_dir_all(&sandbox).await;
    }

    #[tokio::test]
    async fn spec_018_delete_document_rejects_paths_outside_notes_dir() {
        let sandbox = unique_sandbox("delete-safety");
        let notes_dir = sandbox.join("notes");
        let outside_path = sandbox.join("outside.md");
        tokio::fs::create_dir_all(&notes_dir)
            .await
            .expect("notes dir should be created");
        tokio::fs::write(&outside_path, "notes")
            .await
            .expect("outside file should exist");

        let settings = Settings {
            storage: rustle_core::StorageSettings {
                notes_dir: notes_dir.display().to_string(),
                ..Settings::default().storage
            },
            ..Settings::default()
        };

        let error = delete_document(&settings, StoredDocumentKind::Note, &outside_path)
            .await
            .expect_err("outside file should be rejected");

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(tokio::fs::try_exists(&outside_path).await.unwrap_or(false));

        let _ = tokio::fs::remove_dir_all(&sandbox).await;
    }

    fn unique_sandbox(test_name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("rustle-storage-{test_name}-{unique}"))
    }

    #[test]
    fn render_markdown_generates_structure_when_markdown_is_empty() {
        let notes = MeetingNotes {
            summary: "We discussed the roadmap.".to_owned(),
            key_decisions: Vec::new(),
            action_items: Vec::new(),
            attendees: Vec::new(),
            markdown: String::new(),
        };

        let rendered = render_markdown(&notes);
        assert!(rendered.contains("# Meeting Notes"));
        assert!(rendered.contains("## Summary"));
        assert!(rendered.contains("We discussed the roadmap."));
    }

    #[test]
    fn parse_document_returns_none_for_invalid_filename() {
        let result = parse_document(PathBuf::from("/"), StoredDocumentKind::Note);
        assert!(result.is_none());
    }

    #[test]
    fn humanise_title_converts_underscores_to_spaces() {
        assert_eq!(humanise_title("team_sync"), "team sync");
    }
}
