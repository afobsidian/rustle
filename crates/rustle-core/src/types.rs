//! Domain types shared across Rustle crates.

use serde::{Deserialize, Serialize};

/// Source that detected a meeting state change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DetectionSource {
    /// A Hyprland window or IPC event indicated a meeting.
    Hyprland,
    /// PipeWire or PulseAudio stream state indicated a meeting.
    PipeWire,
    /// Process polling indicated a meeting.
    Process,
    /// The user manually created or controlled the meeting state.
    Manual,
}

/// A timestamped segment of meeting transcript text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptSegment {
    /// Segment start time in milliseconds from the beginning of the recording.
    pub start_ms: u64,
    /// Segment end time in milliseconds from the beginning of the recording.
    pub end_ms: u64,
    /// Transcribed text for this segment.
    pub text: String,
}

/// Structured notes produced for a meeting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeetingNotes {
    /// Short meeting summary.
    pub summary: String,
    /// Decisions made during the meeting.
    pub key_decisions: Vec<String>,
    /// Action items identified during the meeting.
    pub action_items: Vec<String>,
    /// Attendees detected or provided for the meeting.
    pub attendees: Vec<String>,
    /// Markdown rendering of the complete note.
    pub markdown: String,
}
