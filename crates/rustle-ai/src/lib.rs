//! AI summarisation crate for Rustle.

#[cfg(feature = "provider-bench")]
pub mod benchmark;

mod providers;

use std::collections::HashMap;

use providers::{AiRuntime, SummarisationOutcome};
use rustle_core::{
    tasks::spawn_logged, AppEvent, CoreError, EventReceiver, EventSender, NotificationUrgency,
    Settings, TranscriptSegment,
};
use tracing::{info, warn};

/// Initialises the AI summarisation component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    if let Some(receiver) = event_rx {
        spawn_logged(
            "ai-summarisation-loop",
            summarisation_loop(event_tx, receiver),
        );
    }

    Ok(())
}

async fn summarisation_loop(event_tx: EventSender, mut event_rx: EventReceiver) {
    let mut runtime = AiRuntime;
    let mut meeting_names = HashMap::new();
    let startup_settings = load_settings("startup AI warmup").await;
    runtime.warm_up(&startup_settings);

    loop {
        tokio::select! {
            event = event_rx.recv() => match event {
                Ok(AppEvent::MeetingStarted { id, name, .. }) => {
                    meeting_names.insert(id, name);
                }
                Ok(AppEvent::TranscriptionReady {
                    meeting_id,
                    segments,
                }) => {
                    let settings = load_settings("AI summarisation").await;
                    let transcript = transcript_text(&segments);
                    let meeting_name = meeting_names
                        .get(&meeting_id)
                        .cloned()
                        .unwrap_or_else(|| "Meeting".to_owned());
                    info!(
                        meeting_id = %meeting_id,
                        provider = ?settings.ai.provider,
                        segments = segments.len(),
                        transcript_chars = transcript.chars().count(),
                        "summarising transcript"
                    );
                    summarise(
                        &event_tx,
                        &mut runtime,
                        meeting_id,
                        &meeting_name,
                        settings,
                        transcript,
                    )
                    .await;
                }
                Ok(AppEvent::SettingsChanged(settings)) => {
                    runtime.warm_up(&settings.validated());
                }
                Ok(AppEvent::QuitRequested) => break,
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "AI loop lagged behind event bus");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}

async fn summarise(
    event_tx: &EventSender,
    runtime: &mut AiRuntime,
    meeting_id: uuid::Uuid,
    meeting_name: &str,
    settings: Settings,
    transcript: String,
) {
    match runtime.summarise(&settings, &transcript).await {
        SummarisationOutcome::Ready { notes, warning } => {
            info!(meeting_id = %meeting_id, "summarisation complete");
            if let Some(warning) = warning {
                publish(
                    event_tx,
                    summarisation_warning_notification(meeting_name, &warning),
                );
            }
            publish(event_tx, AppEvent::SummarisationReady { meeting_id, notes });
        }
    }
}

fn summarisation_warning_notification(meeting_name: &str, warning: &str) -> AppEvent {
    AppEvent::NotificationRequested {
        title: "Notes fallback saved".to_owned(),
        body: format!("Rustle saved fallback notes for {meeting_name}. {warning}"),
        urgency: NotificationUrgency::Critical,
    }
}

async fn load_settings(purpose: &'static str) -> Settings {
    match Settings::load().await {
        Ok(settings) => settings,
        Err(error) => {
            warn!(%error, purpose, "failed to load settings; using defaults");
            Settings::default().validated()
        }
    }
}

fn transcript_text(segments: &[TranscriptSegment]) -> String {
    segments
        .iter()
        .map(|segment| segment.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn publish(event_tx: &EventSender, event: AppEvent) {
    if let Err(error) = event_tx.send(event) {
        warn!(%error, "failed to publish event");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarisation_warning_notification_mentions_fallback() {
        let AppEvent::NotificationRequested {
            title,
            body,
            urgency,
        } = summarisation_warning_notification(
            "Roadmap Review",
            "llama.cpp summarisation unavailable: timed out",
        )
        else {
            panic!("expected notification event");
        };

        assert_eq!(title, "Notes fallback saved");
        assert_eq!(urgency, NotificationUrgency::Critical);
        assert!(body.contains("Roadmap Review"));
        assert!(body.contains("fallback notes"));
        assert!(body.contains("timed out"));
    }
}
