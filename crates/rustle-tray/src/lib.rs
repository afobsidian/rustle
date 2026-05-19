//! System tray crate for Rustle.

use std::path::PathBuf;

use ksni::menu::{StandardItem, SubMenu};
use ksni::{Category, Handle, MenuItem, Status, ToolTip, Tray, TrayMethods};
use rustle_core::{
    tasks::spawn_logged, AppEvent, CoreError, DetectionSource, EventReceiver, EventSender,
};
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tracing::{info, warn};
use uuid::Uuid;

const ICON_THEME_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icons");

#[derive(Clone)]
struct RustleTray {
    event_tx: EventSender,
    active_meeting: Option<(Uuid, String)>,
    latest_note: Option<PathBuf>,
    latest_transcript: Option<PathBuf>,
}

/// Initialises the system tray component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    info!("commands: start [meeting name], stop, open, transcript, settings, quit, help");

    spawn_logged(
        "terminal-control-loop",
        terminal_control_loop(event_tx.clone()),
    );

    if let Some(receiver) = event_rx {
        start_status_notifier(event_tx, receiver).await;
    }

    Ok(())
}

async fn start_status_notifier(event_tx: EventSender, receiver: EventReceiver) {
    let tray = RustleTray {
        event_tx,
        active_meeting: None,
        latest_note: None,
        latest_transcript: None,
    };

    match tray.assume_sni_available(true).spawn().await {
        Ok(handle) => {
            info!("system tray registered");
            spawn_logged("tray-status-loop", status_loop(receiver, handle));
        }
        Err(error) => {
            warn!(%error, "desktop tray integration unavailable; terminal controls remain active");
            spawn_logged("tray-status-loop", terminal_status_loop(receiver));
        }
    }
}

impl Tray for RustleTray {
    fn id(&self) -> String {
        "rustle".to_owned()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    fn title(&self) -> String {
        "Rustle".to_owned()
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_theme_path(&self) -> String {
        ICON_THEME_PATH.to_owned()
    }

    fn icon_name(&self) -> String {
        if self.active_meeting.is_some() {
            "rustle-recording".to_owned()
        } else {
            "rustle".to_owned()
        }
    }

    fn tool_tip(&self) -> ToolTip {
        let description = self
            .active_meeting
            .as_ref()
            .map(|(_, name)| format!("Current meeting: {name}"))
            .unwrap_or_else(|| "No active meeting".to_owned());

        ToolTip {
            icon_name: self.icon_name(),
            title: "Rustle".to_owned(),
            description,
            ..ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        publish(&self.event_tx, AppEvent::OpenNotesRequested);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            standard_item("_Open Notes", true, "document-open", |tray| {
                publish(&tray.event_tx, AppEvent::OpenNotesRequested);
            }),
            standard_item(
                "Open _Transcript",
                self.latest_transcript.is_some(),
                "text-x-generic",
                |tray| publish(&tray.event_tx, AppEvent::OpenTranscriptRequested),
            ),
            current_meeting_item(self.active_meeting.as_ref()),
            MenuItem::Separator,
            meeting_submenu(self.active_meeting.is_some()),
            MenuItem::Separator,
            standard_item("_Settings", true, "preferences-system", |tray| {
                publish(&tray.event_tx, AppEvent::OpenSettingsRequested);
            }),
            standard_item("_Quit", true, "application-exit", |tray| {
                tray.stop_active_meeting();
                publish(&tray.event_tx, AppEvent::QuitRequested);
            }),
        ]
    }

    fn watcher_online(&self) {
        info!("system tray watcher is online");
    }

    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        warn!(
            ?reason,
            "system tray watcher is offline; waiting for it to return"
        );
        true
    }
}

impl RustleTray {
    fn start_manual_meeting(&mut self) {
        if self.active_meeting.is_some() {
            warn!("a meeting is already active");
            return;
        }

        let meeting_id = Uuid::new_v4();
        let meeting_name = format!("Manual Meeting {}", unix_timestamp_seconds());
        self.active_meeting = Some((meeting_id, meeting_name.clone()));
        publish(
            &self.event_tx,
            AppEvent::MeetingStarted {
                id: meeting_id,
                name: meeting_name,
                source: DetectionSource::Manual,
            },
        );
    }

    fn stop_active_meeting(&mut self) {
        let Some((meeting_id, meeting_name)) = self.active_meeting.take() else {
            warn!("no meeting is active");
            return;
        };

        info!(meeting = %meeting_name, "meeting stopped");
        publish(&self.event_tx, AppEvent::MeetingEnded { id: meeting_id });
    }
}

fn standard_item(
    label: &'static str,
    enabled: bool,
    icon_name: &'static str,
    activate: impl Fn(&mut RustleTray) + Send + 'static,
) -> MenuItem<RustleTray> {
    StandardItem {
        label: label.to_owned(),
        enabled,
        icon_name: icon_name.to_owned(),
        activate: Box::new(activate),
        ..StandardItem::default()
    }
    .into()
}

fn current_meeting_item(active_meeting: Option<&(Uuid, String)>) -> MenuItem<RustleTray> {
    let label = active_meeting
        .map(|(_, name)| format!("Current _Meeting: {name}"))
        .unwrap_or_else(|| "Current _Meeting: None".to_owned());

    StandardItem {
        label,
        enabled: false,
        icon_name: "audio-input-microphone".to_owned(),
        ..StandardItem::default()
    }
    .into()
}

fn meeting_submenu(meeting_active: bool) -> MenuItem<RustleTray> {
    SubMenu {
        label: "_Meeting".to_owned(),
        icon_name: "audio-input-microphone".to_owned(),
        submenu: vec![
            standard_item(
                "_Start Manual Meeting",
                !meeting_active,
                "media-record",
                |tray| {
                    tray.start_manual_meeting();
                },
            ),
            standard_item(
                "S_top Meeting",
                meeting_active,
                "media-playback-stop",
                |tray| {
                    tray.stop_active_meeting();
                },
            ),
        ],
        ..SubMenu::default()
    }
    .into()
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
            "open" | "notes" => publish(&event_tx, AppEvent::OpenNotesRequested),
            "transcript" => publish(&event_tx, AppEvent::OpenTranscriptRequested),
            "settings" => publish(&event_tx, AppEvent::OpenSettingsRequested),
            "quit" | "exit" => {
                if let Some((meeting_id, _meeting_name)) = active_meeting.take() {
                    publish(&event_tx, AppEvent::MeetingEnded { id: meeting_id });
                }
                publish(&event_tx, AppEvent::QuitRequested);
                break;
            }
            "help" => info!(
                "commands: start [meeting name], stop, open, transcript, settings, quit, help"
            ),
            unknown => warn!(command = unknown, "unknown terminal control command"),
        }
    }
}

async fn status_loop(mut event_rx: EventReceiver, handle: Handle<RustleTray>) {
    loop {
        match event_rx.recv().await {
            Ok(AppEvent::MeetingStarted { id, name, source }) => {
                info!(?source, meeting = %name, "meeting started");
                update_tray(&handle, |tray| tray.active_meeting = Some((id, name))).await;
            }
            Ok(AppEvent::MeetingEnded { id }) => {
                update_tray(&handle, |tray| {
                    if tray
                        .active_meeting
                        .as_ref()
                        .is_some_and(|(meeting_id, _)| *meeting_id == id)
                    {
                        tray.active_meeting = None;
                    }
                })
                .await;
            }
            Ok(AppEvent::TranscriptDraftReady { path, .. }) => {
                info!(path = %path.display(), "transcript draft ready");
                update_tray(&handle, |tray| tray.latest_transcript = Some(path)).await;
            }
            Ok(AppEvent::NoteSaved { path, .. }) => {
                info!(path = %path.display(), "note saved");
                update_tray(&handle, |tray| tray.latest_note = Some(path)).await;
            }
            Ok(AppEvent::QuitRequested) => {
                handle.shutdown().await;
                break;
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "tray status loop lagged behind event bus");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn update_tray(handle: &Handle<RustleTray>, update: impl FnOnce(&mut RustleTray)) {
    if handle.update(update).await.is_none() {
        warn!("system tray update skipped because tray service is closed");
    }
}

async fn terminal_status_loop(mut event_rx: EventReceiver) {
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
