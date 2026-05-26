//! Shared Rustle types, settings, errors, and event bus primitives.

pub mod environment;
pub mod errors;
pub mod events;
pub mod paths;
pub mod settings;
pub mod tasks;
pub mod types;

pub use environment::{is_hyprland_session, supported_session_label};
pub use errors::CoreError;
pub use events::{
    AppEvent, EventBus, EventPublishError, EventReceiver, EventSender, NotificationUrgency,
    EVENT_BUS_CAPACITY,
};
pub use paths::{
    default_data_dir, expand_tilde, is_safe_child, resolve_notes_dir, resolve_transcripts_dir,
    safe_filename,
};
pub use settings::{
    default_config_path, AiProvider, AiSettings, AudioSettings, GeneralSettings, LoginMethod,
    MeetingSettings, RecordingDetectionMethod, Settings, StorageSettings, TranscriptionMethod,
    TranscriptionSettings,
};
pub use types::{
    DetectionSource, MeetingNotes, StoredDocument, StoredDocumentKind, TranscriptSegment,
};
