//! Meeting detection crate for Rustle.

use std::path::PathBuf;
use std::time::Duration;

use rustle_core::{
    is_hyprland_session, supported_session_label, tasks::spawn_logged, AppEvent, CoreError,
    DetectionSource, EventReceiver, EventSender, Settings,
};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};
use uuid::Uuid;

const HYPRLAND_SOCKET_NAME: &str = ".socket2.sock";
const HYPRLAND_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const HYPRLAND_MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);
const HYPRLAND_POLL_INTERVAL: Duration = Duration::from_secs(15);
const NON_MEETING_TITLE_PREFIXES: &[&str] = &[
    "activity",
    "calendar",
    "calls",
    "chat",
    "files",
    "teams",
    "tasks",
    "communities",
    "more",
];
const TEAMS_TITLE_SUFFIXES: &[&str] = &[
    " | Microsoft Teams",
    " - Microsoft Teams",
    " – Microsoft Teams",
    " — Microsoft Teams",
    " | teams.microsoft.com",
    " - teams.microsoft.com",
    " – teams.microsoft.com",
    " — teams.microsoft.com",
];
const BROWSER_TITLE_SUFFIXES: &[&str] = &[
    " | Google Chrome",
    " - Google Chrome",
    " | Chromium",
    " - Chromium",
    " | Mozilla Firefox",
    " - Mozilla Firefox",
    " | Firefox",
    " - Firefox",
    " | Brave",
    " - Brave",
    " | Zen Browser",
    " - Zen Browser",
    " | Microsoft Edge",
    " - Microsoft Edge",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct DetectedMeeting {
    id: Uuid,
    address: String,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MeetingCandidate {
    address: String,
    name: String,
    score: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenerMode {
    Polling,
    Connected,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct HyprlandClient {
    #[serde(default)]
    address: String,
    #[serde(rename = "class", default)]
    class_name: String,
    #[serde(default)]
    initial_class: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    initial_title: String,
    #[serde(default = "default_true")]
    mapped: bool,
    #[serde(default)]
    hidden: bool,
}

/// Initialises the meeting detection component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    if let Some(receiver) = event_rx {
        spawn_logged(
            "hyprland-detection-loop",
            detection_loop(event_tx, receiver),
        );
    }

    Ok(())
}

async fn detection_loop(event_tx: EventSender, mut event_rx: EventReceiver) {
    let mut settings = match Settings::load().await {
        Ok(settings) => settings,
        Err(error) => {
            warn!(%error, "failed to load settings for meeting detection; using defaults");
            Settings::default().validated()
        }
    };

    info!(
        auto_detect = settings.meeting.auto_detect,
        method = ?settings.meeting.detection_method,
        "automatic Hyprland meeting detection initialised"
    );

    if !is_hyprland_session() {
        warn!(
            expected_session = supported_session_label(),
            "automatic meeting detection is disabled because the current session is unsupported"
        );
    }

    let (trigger_tx, mut trigger_rx) = mpsc::unbounded_channel();
    if is_hyprland_session() {
        spawn_logged(
            "hyprland-event-listener",
            hyprland_event_listener(trigger_tx.clone()),
        );
        let _ = trigger_tx.send(());
    }

    let mut poll_interval = tokio::time::interval(HYPRLAND_POLL_INTERVAL);
    poll_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut active_meeting: Option<DetectedMeeting> = None;
    let mut external_meeting_id: Option<Uuid> = None;

    loop {
        tokio::select! {
            event = event_rx.recv() => match event {
                Ok(AppEvent::MeetingStarted { id, source, .. }) => {
                    if source != DetectionSource::Hyprland {
                        external_meeting_id = Some(id);
                        end_detected_meeting(&event_tx, &mut active_meeting);
                    }
                }
                Ok(AppEvent::MeetingEnded { id }) => {
                    if external_meeting_id == Some(id) {
                        external_meeting_id = None;
                        let _ = trigger_tx.send(());
                    }
                }
                Ok(AppEvent::SettingsChanged(updated_settings)) => {
                    let was_enabled = detection_enabled(&settings);
                    settings = updated_settings;
                    let is_enabled = detection_enabled(&settings);

                    if !is_enabled {
                        end_detected_meeting(&event_tx, &mut active_meeting);
                    }

                    if !was_enabled && is_enabled {
                        let _ = trigger_tx.send(());
                    }
                }
                Ok(AppEvent::QuitRequested) => {
                    end_detected_meeting(&event_tx, &mut active_meeting);
                    break;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "Hyprland detection loop lagged behind event bus");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            trigger = trigger_rx.recv() => {
                if trigger.is_none() {
                    continue;
                }

                if detection_enabled(&settings) {
                    reconcile_detected_meeting(
                        &event_tx,
                        &mut active_meeting,
                        external_meeting_id,
                    )
                    .await;
                }
            }
            _ = poll_interval.tick(), if detection_enabled(&settings) => {
                reconcile_detected_meeting(&event_tx, &mut active_meeting, external_meeting_id)
                    .await;
            }
        }
    }
}

async fn reconcile_detected_meeting(
    event_tx: &EventSender,
    active_meeting: &mut Option<DetectedMeeting>,
    external_meeting_id: Option<Uuid>,
) {
    if external_meeting_id.is_some() {
        end_detected_meeting(event_tx, active_meeting);
        return;
    }

    let candidate = match snapshot_meeting_candidate().await {
        Ok(candidate) => candidate,
        Err(error) => {
            warn!(%error, "failed to inspect Hyprland clients for meeting detection");
            return;
        }
    };

    match (active_meeting.as_mut(), candidate) {
        (None, Some(candidate)) => {
            let meeting = DetectedMeeting {
                id: Uuid::new_v4(),
                address: candidate.address,
                name: candidate.name,
            };
            info!(meeting = %meeting.name, "detected meeting through Hyprland");
            publish(
                event_tx,
                AppEvent::MeetingStarted {
                    id: meeting.id,
                    name: meeting.name.clone(),
                    source: DetectionSource::Hyprland,
                },
            );
            *active_meeting = Some(meeting);
        }
        (Some(meeting), Some(candidate)) if meeting.address == candidate.address => {
            if meeting.name != candidate.name {
                info!(meeting = %candidate.name, "Hyprland meeting title updated");
                meeting.name = candidate.name;
            }
        }
        (Some(_), Some(candidate)) => {
            end_detected_meeting(event_tx, active_meeting);
            let meeting = DetectedMeeting {
                id: Uuid::new_v4(),
                address: candidate.address,
                name: candidate.name,
            };
            info!(meeting = %meeting.name, "detected replacement Hyprland meeting window");
            publish(
                event_tx,
                AppEvent::MeetingStarted {
                    id: meeting.id,
                    name: meeting.name.clone(),
                    source: DetectionSource::Hyprland,
                },
            );
            *active_meeting = Some(meeting);
        }
        (Some(_), None) => {
            end_detected_meeting(event_tx, active_meeting);
        }
        (None, None) => {}
    }
}

fn end_detected_meeting(event_tx: &EventSender, active_meeting: &mut Option<DetectedMeeting>) {
    if let Some(meeting) = active_meeting.take() {
        info!(meeting = %meeting.name, "Hyprland meeting ended");
        publish(event_tx, AppEvent::MeetingEnded { id: meeting.id });
    }
}

async fn snapshot_meeting_candidate() -> Result<Option<MeetingCandidate>, String> {
    let output = Command::new("hyprctl")
        .args(["clients", "-j"])
        .output()
        .await
        .map_err(|error| error.to_string())?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(format!(
            "hyprctl clients -j exited with {}: {stderr}",
            output.status
        ));
    }

    let clients = serde_json::from_slice::<Vec<HyprlandClient>>(&output.stdout)
        .map_err(|error| error.to_string())?;

    Ok(select_meeting_candidate(&clients))
}

fn select_meeting_candidate(clients: &[HyprlandClient]) -> Option<MeetingCandidate> {
    clients
        .iter()
        .filter(|client| client.mapped && !client.hidden)
        .filter_map(meeting_candidate_from_client)
        .max_by_key(|candidate| candidate.score)
}

fn meeting_candidate_from_client(client: &HyprlandClient) -> Option<MeetingCandidate> {
    let address = client.address.trim();
    if address.is_empty() {
        return None;
    }

    let name = meeting_name_from_client(client);
    if !is_meeting_title_allowed(&name) {
        return None;
    }

    let title_score = title_match_score(client);
    let class_score = class_match_score(client);
    let score = title_score.max(class_score)?;

    Some(MeetingCandidate {
        address: address.to_owned(),
        name,
        score,
    })
}

fn title_match_score(client: &HyprlandClient) -> Option<u8> {
    let title = client.title.to_ascii_lowercase();
    let initial_title = client.initial_title.to_ascii_lowercase();

    if title.contains("microsoft teams")
        || initial_title.contains("microsoft teams")
        || title.contains("teams.microsoft.com")
        || initial_title.contains("teams.microsoft.com")
    {
        Some(2)
    } else {
        None
    }
}

fn class_match_score(client: &HyprlandClient) -> Option<u8> {
    let class_name = client.class_name.to_ascii_lowercase();
    let initial_class = client.initial_class.to_ascii_lowercase();

    if class_name.contains("teams") || initial_class.contains("teams") {
        Some(1)
    } else {
        None
    }
}

fn meeting_name_from_client(client: &HyprlandClient) -> String {
    normalize_meeting_name(&client.title)
        .or_else(|| normalize_meeting_name(&client.initial_title))
        .unwrap_or_else(|| "Microsoft Teams".to_owned())
}

fn normalize_meeting_name(raw_title: &str) -> Option<String> {
    let trimmed = raw_title.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut normalized = trimmed.to_owned();
    loop {
        let stripped = strip_known_suffixes(&normalized, BROWSER_TITLE_SUFFIXES)
            .or_else(|| strip_known_suffixes(&normalized, TEAMS_TITLE_SUFFIXES));

        match stripped {
            Some(next) if next != normalized => normalized = next,
            _ => break,
        }
    }

    if normalized.is_empty() {
        return Some("Microsoft Teams".to_owned());
    }

    let lowercase = normalized.to_ascii_lowercase();
    if lowercase == "microsoft teams" || lowercase.contains("teams.microsoft.com") {
        return Some("Microsoft Teams".to_owned());
    }

    Some(normalized)
}

fn strip_known_suffixes(value: &str, suffixes: &[&str]) -> Option<String> {
    suffixes.iter().find_map(|suffix| {
        value
            .strip_suffix(suffix)
            .map(|stripped| stripped.trim().to_owned())
    })
}

fn is_meeting_title_allowed(name: &str) -> bool {
    let lowered = name.trim().to_ascii_lowercase();
    if lowered.is_empty() || lowered == "microsoft teams" {
        return false;
    }

    let prefix = lowered.split('|').next().map(str::trim).unwrap_or_default();

    !NON_MEETING_TITLE_PREFIXES.contains(&prefix)
}

async fn hyprland_event_listener(trigger_tx: mpsc::UnboundedSender<()>) {
    let mut mode = ListenerMode::Polling;
    let mut reconnect_delay = HYPRLAND_RECONNECT_DELAY;

    loop {
        let Some(socket_path) = hyprland_socket_path() else {
            log_listener_transition(
                &mut mode,
                ListenerMode::Polling,
                "Hyprland socket path unavailable; staying in polling mode",
            );
            tokio::time::sleep(reconnect_delay).await;
            reconnect_delay = next_reconnect_delay(reconnect_delay);
            continue;
        };

        match UnixStream::connect(&socket_path).await {
            Ok(stream) => {
                log_listener_transition(
                    &mut mode,
                    ListenerMode::Connected,
                    format!(
                        "connected to Hyprland event socket at {}",
                        socket_path.display()
                    ),
                );
                reconnect_delay = HYPRLAND_RECONNECT_DELAY;
                if read_hyprland_events(stream, &trigger_tx).await {
                    return;
                }
                log_listener_transition(
                    &mut mode,
                    ListenerMode::Polling,
                    format!(
                        "Hyprland event socket at {} closed; falling back to polling",
                        socket_path.display()
                    ),
                );
            }
            Err(error) => {
                log_listener_transition(
                    &mut mode,
                    ListenerMode::Polling,
                    format!(
                        "failed to connect to Hyprland event socket at {}: {error}; falling back to polling",
                        socket_path.display()
                    ),
                );
            }
        }

        tokio::time::sleep(reconnect_delay).await;
        reconnect_delay = next_reconnect_delay(reconnect_delay);
    }
}

async fn read_hyprland_events(stream: UnixStream, trigger_tx: &mpsc::UnboundedSender<()>) -> bool {
    let mut lines = BufReader::new(stream).lines();

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if is_relevant_hyprland_event(&line) && trigger_tx.send(()).is_err() {
                    return true;
                }
            }
            Ok(None) => return false,
            Err(error) => {
                warn!(%error, "failed to read Hyprland event stream");
                return false;
            }
        }
    }
}

fn hyprland_socket_path() -> Option<PathBuf> {
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .ok()
        .filter(|value| !value.is_empty())?;

    hyprland_socket_candidates(&signature).into_iter().next()
}

fn hyprland_socket_path_from_signature(signature: &str) -> PathBuf {
    PathBuf::from("/tmp/hypr")
        .join(signature)
        .join(HYPRLAND_SOCKET_NAME)
}

fn hyprland_socket_candidates(signature: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::with_capacity(2);

    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty())
    {
        candidates.push(
            PathBuf::from(runtime_dir)
                .join("hypr")
                .join(signature)
                .join(HYPRLAND_SOCKET_NAME),
        );
    }

    let legacy_path = hyprland_socket_path_from_signature(signature);
    if !candidates.iter().any(|path| path == &legacy_path) {
        candidates.push(legacy_path);
    }

    candidates.sort_by_key(|path| !path.exists());
    candidates
}

fn is_relevant_hyprland_event(line: &str) -> bool {
    let Some((event_name, _payload)) = line.split_once(">>") else {
        return false;
    };

    matches!(
        event_name,
        "openwindow"
            | "closewindow"
            | "windowtitle"
            | "windowtitlev2"
            | "activewindow"
            | "activewindowv2"
    )
}

fn next_reconnect_delay(current: Duration) -> Duration {
    current
        .saturating_mul(2)
        .min(HYPRLAND_MAX_RECONNECT_DELAY)
        .max(HYPRLAND_RECONNECT_DELAY)
}

fn log_listener_transition(
    mode: &mut ListenerMode,
    next_mode: ListenerMode,
    message: impl Into<String>,
) {
    let message = message.into();
    if *mode == next_mode {
        info!(mode = ?next_mode, "{message}");
        return;
    }

    match next_mode {
        ListenerMode::Connected => {
            info!(from = ?*mode, to = ?next_mode, "Hyprland socket listener recovered: {message}");
        }
        ListenerMode::Polling => {
            warn!(from = ?*mode, to = ?next_mode, "Hyprland socket listener degraded: {message}");
        }
    }
    *mode = next_mode;
}

fn detection_enabled(settings: &Settings) -> bool {
    settings.meeting.auto_detect && is_hyprland_session()
}

fn publish(event_tx: &EventSender, event: AppEvent) {
    if let Err(error) = event_tx.send(event) {
        warn!(%error, "failed to publish event");
    }
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(address: &str, class_name: &str, title: &str) -> HyprlandClient {
        HyprlandClient {
            address: address.to_owned(),
            class_name: class_name.to_owned(),
            initial_class: class_name.to_owned(),
            title: title.to_owned(),
            initial_title: title.to_owned(),
            mapped: true,
            hidden: false,
        }
    }

    #[test]
    fn detects_meeting_candidate_from_title_match() {
        let clients = vec![
            client("0x123", "firefox", "Docs"),
            client("0x456", "chromium", "Planning Sync | Microsoft Teams"),
        ];

        let candidate = select_meeting_candidate(&clients).expect("meeting candidate should exist");

        assert_eq!(candidate.address, "0x456");
        assert_eq!(candidate.name, "Planning Sync");
        assert_eq!(candidate.score, 2);
    }

    #[test]
    fn allows_class_only_match_for_non_navigation_titles() {
        let clients = vec![client("0xabc", "teams-for-linux", "Daily Sync")];

        let candidate = select_meeting_candidate(&clients).expect("meeting candidate should exist");

        assert_eq!(candidate.address, "0xabc");
        assert_eq!(candidate.name, "Daily Sync");
        assert_eq!(candidate.score, 1);
    }

    #[test]
    fn strips_common_teams_title_suffixes() {
        assert_eq!(
            normalize_meeting_name("Weekly Sync | Microsoft Teams"),
            Some("Weekly Sync".to_owned())
        );
        assert_eq!(
            normalize_meeting_name("Standup - Microsoft Teams"),
            Some("Standup".to_owned())
        );
    }

    #[test]
    fn strips_browser_and_web_suffixes_from_teams_titles() {
        assert_eq!(
            normalize_meeting_name("Planning Sync | Microsoft Teams | Google Chrome"),
            Some("Planning Sync".to_owned())
        );
        assert_eq!(
            normalize_meeting_name("Design Review | teams.microsoft.com - Chromium"),
            Some("Design Review".to_owned())
        );
    }

    #[test]
    fn rejects_non_meeting_navigation_titles() {
        let clients = vec![client(
            "0xabc",
            "teams-for-linux",
            "Calendar | Brent Wallace | Microsoft Teams",
        )];

        assert_eq!(select_meeting_candidate(&clients), None);
        assert!(!is_meeting_title_allowed("Calendar | Brent Wallace"));
        assert!(!is_meeting_title_allowed("Microsoft Teams"));
        assert!(!is_meeting_title_allowed("Chat | teams.microsoft.com"));
    }

    #[test]
    fn accepts_regular_meeting_titles_without_navigation_prefixes() {
        let clients = vec![client(
            "0xabc",
            "teams-for-linux",
            "Sprint Planning | Microsoft Teams",
        )];

        let candidate = select_meeting_candidate(&clients).expect("meeting candidate should exist");

        assert_eq!(candidate.name, "Sprint Planning");
        assert!(is_meeting_title_allowed("Sprint Planning"));
    }

    #[test]
    fn detects_web_meeting_titles_with_browser_suffixes() {
        let clients = vec![client(
            "0xabc",
            "chromium",
            "Roadmap Review | Microsoft Teams | Google Chrome",
        )];

        let candidate = select_meeting_candidate(&clients).expect("meeting candidate should exist");

        assert_eq!(candidate.name, "Roadmap Review");
        assert_eq!(candidate.score, 2);
    }

    #[test]
    fn filters_relevant_hyprland_events() {
        assert!(is_relevant_hyprland_event("openwindow>>payload"));
        assert!(is_relevant_hyprland_event("windowtitlev2>>payload"));
        assert!(!is_relevant_hyprland_event("workspace>>payload"));
        assert!(!is_relevant_hyprland_event("malformed"));
    }

    #[test]
    fn builds_hyprland_socket_path_from_signature() {
        assert_eq!(
            hyprland_socket_path_from_signature("abc123"),
            PathBuf::from("/tmp/hypr/abc123/.socket2.sock")
        );
    }

    #[test]
    fn prefers_xdg_runtime_dir_candidate() {
        let original_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR");

        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        }

        let candidates = hyprland_socket_candidates("abc123");

        assert_eq!(
            candidates.first(),
            Some(&PathBuf::from("/run/user/1000/hypr/abc123/.socket2.sock"))
        );
        assert!(candidates.contains(&PathBuf::from("/tmp/hypr/abc123/.socket2.sock")));

        match original_runtime_dir {
            Some(value) => unsafe {
                std::env::set_var("XDG_RUNTIME_DIR", value);
            },
            None => unsafe {
                std::env::remove_var("XDG_RUNTIME_DIR");
            },
        }
    }

    #[test]
    fn reconnect_delay_backs_off_and_caps() {
        assert_eq!(
            next_reconnect_delay(Duration::from_secs(1)),
            HYPRLAND_RECONNECT_DELAY
        );
        assert_eq!(
            next_reconnect_delay(HYPRLAND_RECONNECT_DELAY),
            Duration::from_secs(10)
        );
        assert_eq!(
            next_reconnect_delay(Duration::from_secs(45)),
            HYPRLAND_MAX_RECONNECT_DELAY
        );
        assert_eq!(
            next_reconnect_delay(HYPRLAND_MAX_RECONNECT_DELAY),
            HYPRLAND_MAX_RECONNECT_DELAY
        );
    }
}
