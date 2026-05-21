//! Broadcast event bus used for inter-crate communication.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::settings::Settings;
use crate::types::{DetectionSource, MeetingNotes, TranscriptSegment};

/// Number of events retained by the application broadcast channel.
pub const EVENT_BUS_CAPACITY: usize = 64;

/// Sender side of the Rustle application event bus.
pub type EventSender = broadcast::Sender<AppEvent>;

/// Receiver side of the Rustle application event bus.
pub type EventReceiver = broadcast::Receiver<AppEvent>;

/// Error returned when an application event cannot be published.
pub type EventPublishError = Box<broadcast::error::SendError<AppEvent>>;

/// Application-wide event exchanged over the internal broadcast bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppEvent {
    /// A meeting has started.
    MeetingStarted {
        /// Stable meeting identifier.
        id: Uuid,
        /// Human-readable meeting name.
        name: String,
        /// Detection mechanism that found the meeting.
        source: DetectionSource,
    },
    /// A meeting has ended.
    MeetingEnded {
        /// Stable meeting identifier.
        id: Uuid,
    },
    /// Audio recording has started for a meeting.
    RecordingStarted {
        /// Meeting identifier associated with the recording.
        meeting_id: Uuid,
    },
    /// A recording chunk is ready for downstream processing.
    RecordingChunkReady {
        /// Meeting identifier associated with the recording chunk.
        meeting_id: Uuid,
        /// Filesystem path to the completed recording chunk.
        path: PathBuf,
    },
    /// Audio recording has stopped for a meeting.
    RecordingStopped {
        /// Meeting identifier associated with the recording.
        meeting_id: Uuid,
    },
    /// Transcription output is ready for a meeting.
    TranscriptionReady {
        /// Meeting identifier associated with the transcript.
        meeting_id: Uuid,
        /// Timestamped transcript segments.
        segments: Vec<TranscriptSegment>,
    },
    /// AI-generated notes are ready for a meeting.
    SummarisationReady {
        /// Meeting identifier associated with the notes.
        meeting_id: Uuid,
        /// Structured meeting notes.
        notes: MeetingNotes,
    },
    /// The notes view should be opened.
    OpenNotesRequested,
    /// The latest transcript draft should be opened.
    OpenTranscriptRequested,
    /// The settings editor should be opened.
    OpenSettingsRequested,
    /// A specific file or directory should be opened.
    OpenPathRequested {
        /// Filesystem path to open.
        path: PathBuf,
        /// Whether the user's preferred editor should be used before `xdg-open`.
        prefer_editor: bool,
    },
    /// A manual transcript draft has been created for a meeting.
    TranscriptDraftReady {
        /// Meeting identifier associated with the transcript draft.
        meeting_id: Uuid,
        /// Filesystem path to the editable transcript draft.
        path: PathBuf,
    },
    /// Meeting notes have been persisted.
    NoteSaved {
        /// Meeting identifier associated with the saved note.
        meeting_id: Uuid,
        /// Filesystem path to the saved Markdown note.
        path: PathBuf,
    },
    /// A persisted document should be deleted.
    DeleteDocumentRequested {
        /// Filesystem path to delete.
        path: PathBuf,
        /// The document kind, used for validation and tray refresh logic.
        kind: crate::types::StoredDocumentKind,
    },
    /// A persisted document has been deleted.
    DocumentDeleted {
        /// Filesystem path that was deleted.
        path: PathBuf,
        /// The document kind that was deleted.
        kind: crate::types::StoredDocumentKind,
    },
    /// An existing transcript should be summarised into notes.
    SummariseTranscriptRequested {
        /// Filesystem path to the transcript file.
        path: PathBuf,
    },
    /// Settings have changed and should be reloaded by subscribers.
    SettingsChanged(Settings),
    /// The application should shut down.
    QuitRequested,
}

/// Thin wrapper around a Tokio broadcast channel for application events.
#[derive(Debug, Clone)]
pub struct EventBus {
    sender: EventSender,
}

impl EventBus {
    /// Creates a new application event bus with the standard capacity.
    #[must_use]
    pub fn new() -> Self {
        let (sender, _receiver) = broadcast::channel(EVENT_BUS_CAPACITY);
        Self { sender }
    }

    /// Returns a clone of the event sender.
    #[must_use]
    pub fn sender(&self) -> EventSender {
        self.sender.clone()
    }

    /// Subscribes to application events.
    #[must_use]
    pub fn subscribe(&self) -> EventReceiver {
        self.sender.subscribe()
    }

    /// Publishes an event and returns the number of active receivers that accepted it.
    pub fn publish(&self, event: AppEvent) -> Result<usize, EventPublishError> {
        self.sender.send(event).map_err(Box::new)
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let bus = EventBus::new();
        let mut receiver = bus.subscribe();

        let sent = bus.publish(AppEvent::QuitRequested);

        assert_eq!(sent.expect("event should publish"), 1);
        assert!(matches!(
            receiver.recv().await.expect("event should be received"),
            AppEvent::QuitRequested
        ));
    }
}
