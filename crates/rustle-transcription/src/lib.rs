//! Transcription crate for Rustle.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;

use rustle_core::{
    default_data_dir, safe_filename, tasks::spawn_logged, AppEvent, CoreError, EventReceiver,
    EventSender, TranscriptSegment,
};
use tokio::fs;
use tokio::process::Command;
use tracing::{info, warn};

/// Initialises the transcription component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    if let Some(receiver) = event_rx {
        spawn_logged(
            "manual-transcription-loop",
            transcription_loop(event_tx, receiver),
        );
    }

    Ok(())
}

async fn transcription_loop(event_tx: EventSender, mut event_rx: EventReceiver) {
    let mut drafts = HashMap::new();

    loop {
        match event_rx.recv().await {
            Ok(AppEvent::MeetingStarted { id, name, source }) => {
                info!(?source, meeting = %name, "creating manual transcript draft");
                match create_transcript_draft(&name).await {
                    Ok(path) => {
                        drafts.insert(id, path.clone());
                        publish(
                            &event_tx,
                            AppEvent::TranscriptDraftReady {
                                meeting_id: id,
                                path: path.clone(),
                            },
                        );
                        open_path(&path).await;
                    }
                    Err(error) => {
                        warn!(%error, meeting = %name, "failed to create transcript draft")
                    }
                }
            }
            Ok(AppEvent::MeetingEnded { id }) => {
                let Some(path) = drafts.remove(&id) else {
                    warn!(meeting_id = %id, "meeting ended without a transcript draft");
                    publish(
                        &event_tx,
                        AppEvent::TranscriptionReady {
                            meeting_id: id,
                            segments: Vec::new(),
                        },
                    );
                    continue;
                };

                let transcript = match fs::read_to_string(&path).await {
                    Ok(content) => content,
                    Err(error) => {
                        warn!(%error, path = %path.display(), "failed to read transcript draft");
                        String::new()
                    }
                };

                let segments = transcript_segments(transcript);
                info!(
                    meeting_id = %id,
                    path = %path.display(),
                    segments = segments.len(),
                    "transcript ready"
                );
                publish(
                    &event_tx,
                    AppEvent::TranscriptionReady {
                        meeting_id: id,
                        segments,
                    },
                );
            }
            Ok(AppEvent::QuitRequested) => break,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "transcription loop lagged behind event bus");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn create_transcript_draft(meeting_name: &str) -> std::io::Result<PathBuf> {
    let directory = default_data_dir()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?
        .join("transcripts");
    fs::create_dir_all(&directory).await?;

    let file_name = format!(
        "{}_{}.txt",
        unix_timestamp_seconds(),
        safe_filename(meeting_name)
    );
    let path = directory.join(file_name);
    fs::write(&path, indicative_transcript(meeting_name)).await?;
    Ok(path)
}

fn indicative_transcript(meeting_name: &str) -> String {
    let meeting_name = meeting_name.trim();
    let meeting_name = if meeting_name.is_empty() {
        "Manual Meeting"
    } else {
        meeting_name
    };

    format!(
        "Alex: Thanks for joining {meeting_name}. The goal today is to agree the next few steps and make sure nothing important is blocked.\n\nPriya: The current status is mostly positive. The main workflow is in place, but we still need to polish the handoff between capturing notes and reviewing the saved output.\n\nSam: I can take the follow-up to verify the saved note format and check that the important action items are easy to scan.\n\nAlex: Great. Decision one is that we keep the manual transcript flow for the MVP while automatic capture is still being built. Decision two is that the app should always save a useful note even if local AI is still warming up.\n\nPriya: The main risk is that users may not know when summarisation is happening, so progress logging should stay visible during the manual workflow.\n\nSam: Action items are: verify the generated note after this meeting, keep the sample transcript easy to replace, and revisit automatic transcript capture when the audio pipeline lands.\n\nAlex: Sounds good. Let's close with Sam owning note verification, Priya owning workflow polish, and Alex owning the next planning pass.\n"
    )
}

fn transcript_segments(transcript: String) -> Vec<TranscriptSegment> {
    let cleaned = transcript.trim().to_owned();
    if cleaned.is_empty() {
        return Vec::new();
    }

    vec![TranscriptSegment {
        start_ms: 0,
        end_ms: 0,
        text: cleaned,
    }]
}

async fn open_path(path: &PathBuf) {
    match Command::new("xdg-open")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_child) => info!(path = %path.display(), "opened transcript draft"),
        Err(error) => warn!(%error, path = %path.display(), "failed to open transcript draft"),
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

    #[test]
    fn indicative_transcript_includes_meeting_context() {
        let transcript = indicative_transcript("Planning Sync");

        assert!(transcript.contains("Planning Sync"));
        assert!(transcript.contains("Action items"));
    }
}
