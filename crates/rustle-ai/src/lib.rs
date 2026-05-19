//! AI summarisation crate for Rustle.

#[cfg(feature = "provider-bench")]
pub mod benchmark;

mod providers;

use providers::{AiRuntime, SummarisationOutcome};
use rustle_core::{
    tasks::spawn_logged, AppEvent, CoreError, EventReceiver, EventSender, Settings,
    TranscriptSegment,
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
    let mut runtime = AiRuntime::default();
    let startup_settings = load_settings("startup AI warmup").await;
    runtime.warm_up(&startup_settings);

    loop {
        tokio::select! {
            event = event_rx.recv() => match event {
                Ok(AppEvent::TranscriptionReady {
                    meeting_id,
                    segments,
                }) => {
                    let settings = load_settings("AI summarisation").await;
                    let transcript = transcript_text(&segments);
                    info!(
                        meeting_id = %meeting_id,
                        provider = ?settings.ai.provider,
                        segments = segments.len(),
                        transcript_chars = transcript.chars().count(),
                        "summarising transcript"
                    );
                    summarise(&event_tx, &mut runtime, meeting_id, settings, transcript).await;
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
    settings: Settings,
    transcript: String,
) {
    match runtime.summarise(&settings, &transcript).await {
        SummarisationOutcome::Ready(notes) => {
            info!(meeting_id = %meeting_id, "summarisation complete");
            publish(event_tx, AppEvent::SummarisationReady { meeting_id, notes });
        }
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
