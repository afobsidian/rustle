use std::collections::HashMap;
use std::path::Path;

use notify_rust::{Notification, Urgency};
use rustle_core::{
    tasks::spawn_logged, AppEvent, CoreError, DetectionSource, EventReceiver, NotificationUrgency,
};
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotificationPayload {
    title: String,
    body: String,
    urgency: NotificationUrgency,
}

pub async fn initialise(event_rx: Option<EventReceiver>) -> Result<(), CoreError> {
    if let Some(receiver) = event_rx {
        spawn_logged("desktop-notification-loop", notification_loop(receiver));
    }

    Ok(())
}

async fn notification_loop(mut event_rx: EventReceiver) {
    let mut meeting_names = HashMap::new();

    loop {
        match event_rx.recv().await {
            Ok(AppEvent::MeetingStarted { id, name, source }) => {
                meeting_names.insert(id, name.clone());
                dispatch_notification(
                    &meeting_started_notification(&name, source),
                    deliver_notification,
                );
            }
            Ok(AppEvent::RecordingStarted { meeting_id }) => {
                let meeting_name = meeting_names
                    .get(&meeting_id)
                    .map(String::as_str)
                    .unwrap_or("Meeting");
                dispatch_notification(
                    &recording_started_notification(meeting_name),
                    deliver_notification,
                );
            }
            Ok(AppEvent::NoteSaved { meeting_id, path }) => {
                let meeting_name = meeting_names
                    .remove(&meeting_id)
                    .unwrap_or_else(|| "Meeting".to_owned());
                dispatch_notification(
                    &notes_ready_notification(&meeting_name, &path),
                    deliver_notification,
                );
            }
            Ok(AppEvent::NotificationRequested {
                title,
                body,
                urgency,
            }) => {
                dispatch_notification(
                    &NotificationPayload {
                        title,
                        body,
                        urgency,
                    },
                    deliver_notification,
                );
            }
            Ok(AppEvent::QuitRequested) => break,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "desktop notification loop lagged behind event bus");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

fn meeting_started_notification(
    meeting_name: &str,
    source: DetectionSource,
) -> NotificationPayload {
    let (title, body) = match source {
        DetectionSource::Hyprland => (
            "Meeting detected",
            format!("Rustle detected {meeting_name} in Microsoft Teams."),
        ),
        DetectionSource::Manual => ("Meeting started", format!("Rustle started {meeting_name}.")),
    };

    NotificationPayload {
        title: title.to_owned(),
        body,
        urgency: NotificationUrgency::Normal,
    }
}

fn recording_started_notification(meeting_name: &str) -> NotificationPayload {
    NotificationPayload {
        title: "Recording started".to_owned(),
        body: format!("Rustle is recording audio for {meeting_name}."),
        urgency: NotificationUrgency::Normal,
    }
}

fn notes_ready_notification(meeting_name: &str, path: &Path) -> NotificationPayload {
    let destination = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("saved note");

    NotificationPayload {
        title: "Meeting notes ready".to_owned(),
        body: format!("Rustle saved notes for {meeting_name} to {destination}."),
        urgency: NotificationUrgency::Normal,
    }
}

fn dispatch_notification(
    payload: &NotificationPayload,
    deliver: impl FnOnce(&NotificationPayload) -> Result<(), String>,
) -> bool {
    match deliver(payload) {
        Ok(()) => true,
        Err(error) => {
            warn!(title = %payload.title, %error, "failed to deliver desktop notification");
            false
        }
    }
}

fn deliver_notification(payload: &NotificationPayload) -> Result<(), String> {
    Notification::new()
        .appname("Rustle")
        .summary(&payload.title)
        .body(&payload.body)
        .urgency(notification_urgency(payload.urgency))
        .show()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn notification_urgency(urgency: NotificationUrgency) -> Urgency {
    match urgency {
        NotificationUrgency::Low => Urgency::Low,
        NotificationUrgency::Normal => Urgency::Normal,
        NotificationUrgency::Critical => Urgency::Critical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meeting_started_notification_uses_detection_source() {
        let detected = meeting_started_notification("Design Review", DetectionSource::Hyprland);
        let manual = meeting_started_notification("Design Review", DetectionSource::Manual);

        assert_eq!(detected.title, "Meeting detected");
        assert!(detected.body.contains("detected Design Review"));
        assert_eq!(manual.title, "Meeting started");
        assert!(manual.body.contains("started Design Review"));
    }

    #[test]
    fn notes_ready_notification_uses_filename_when_available() {
        let payload = notes_ready_notification(
            "Planning Sync",
            Path::new("/tmp/1779300000_planning_sync.md"),
        );

        assert_eq!(payload.title, "Meeting notes ready");
        assert!(payload.body.contains("Planning Sync"));
        assert!(payload.body.contains("1779300000_planning_sync.md"));
    }

    #[test]
    fn dispatch_notification_returns_false_when_delivery_fails() {
        let payload = recording_started_notification("Weekly Sync");

        let delivered = dispatch_notification(&payload, |_| Err("dbus unavailable".to_owned()));

        assert!(!delivered);
    }
}
