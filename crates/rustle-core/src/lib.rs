//! Shared Rustle types, settings, errors, and event bus primitives.

pub mod errors;
pub mod events;
pub mod settings;
pub mod tasks;
pub mod types;

pub use errors::CoreError;
pub use events::{
    AppEvent, EventBus, EventPublishError, EventReceiver, EventSender, EVENT_BUS_CAPACITY,
};
pub use settings::{
    AiProvider, AiSettings, AudioSettings, GeneralSettings, LoginMethod, MeetingSettings,
    RecordingDetectionMethod, Settings, StorageSettings, TranscriptionMethod,
    TranscriptionSettings,
};
pub use types::{DetectionSource, MeetingNotes, TranscriptSegment};
