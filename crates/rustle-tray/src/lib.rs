//! System tray crate for Rustle.

use rustle_core::{
    tasks::spawn_logged, AppEvent, CoreError, DetectionSource, EventReceiver, EventSender,
};
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tracing::{info, warn};
use uuid::Uuid;

/// Initialises the system tray component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    info!("desktop tray integration is not implemented yet; enabling terminal controls");
    info!("commands: start [meeting name], stop, open, quit, help");

    spawn_logged("terminal-control-loop", terminal_control_loop(event_tx));

    if let Some(receiver) = event_rx {
        spawn_logged("tray-status-loop", status_loop(receiver));
    }

    Ok(())
}

async fn terminal_control_loop(event_tx: EventSender) {
    let stdin = BufReader::new(io::stdin());
    let mut lines = stdin.lines();
    let mut active_meeting: Option<(Uuid, String)> = None;

    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => {
                warn!("terminal controls stopped because stdin closed");
                break;
            }
            Err(error) => {
                warn!(%error, "terminal controls stopped because stdin could not be read");
                break;
            }
        };

        let command = line.trim();
        if command.is_empty() {
            continue;
        }

        let mut parts = command.splitn(2, char::is_whitespace);
        let verb = parts.next().unwrap_or_default().to_ascii_lowercase();
        let argument = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        match verb.as_str() {
            "start" => {
                if active_meeting.is_some() {
                    warn!("a manual meeting is already active; run `stop` before starting another");
                    continue;
                }

                let meeting_id = Uuid::new_v4();
                let meeting_name = argument
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Manual Meeting {}", unix_timestamp_seconds()));

                active_meeting = Some((meeting_id, meeting_name.clone()));
                publish(
                    &event_tx,
                    AppEvent::MeetingStarted {
                        id: meeting_id,
                        name: meeting_name,
                        source: DetectionSource::Manual,
                    },
                );
            }
            "stop" => match active_meeting.take() {
                Some((meeting_id, meeting_name)) => {
                    info!(meeting = %meeting_name, "meeting stopped");
                    publish(&event_tx, AppEvent::MeetingEnded { id: meeting_id });
                }
                None => warn!("no manual meeting is active"),
            },
            "open" => publish(&event_tx, AppEvent::OpenNotesRequested),
            "quit" | "exit" => {
                if let Some((meeting_id, _meeting_name)) = active_meeting.take() {
                    publish(&event_tx, AppEvent::MeetingEnded { id: meeting_id });
                }
                publish(&event_tx, AppEvent::QuitRequested);
                break;
            }
            "help" => info!("commands: start [meeting name], stop, open, quit, help"),
            unknown => warn!(command = unknown, "unknown terminal control command"),
        }
    }
}

async fn status_loop(mut event_rx: EventReceiver) {
    loop {
        match event_rx.recv().await {
            Ok(AppEvent::MeetingStarted { name, source, .. }) => {
                info!(?source, meeting = %name, "meeting started");
            }
            Ok(AppEvent::TranscriptDraftReady { path, .. }) => {
                info!(path = %path.display(), "transcript draft ready");
            }
            Ok(AppEvent::NoteSaved { path, .. }) => {
                info!(path = %path.display(), "note saved");
            }
            Ok(AppEvent::QuitRequested) => break,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "tray status loop lagged behind event bus");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
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
