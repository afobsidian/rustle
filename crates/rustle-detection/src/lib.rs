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
    "meeting compact view",
    "teams and channels",
    "waiting for network...",
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

    apply_detected_candidate(event_tx, active_meeting, candidate);
}

fn apply_detected_candidate(
    event_tx: &EventSender,
    active_meeting: &mut Option<DetectedMeeting>,
    candidate: Option<MeetingCandidate>,
) {
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

    let title_score = title_match_score(client)?;
    let score = title_score;

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

    let normalized = normalized
        .split('|')
        .map(strip_leading_notification_badges)
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");

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

fn strip_leading_notification_badges(value: &str) -> &str {
    let mut trimmed = value.trim();

    while let Some(rest) = trimmed.strip_prefix('(') {
        let Some((count, suffix)) = rest.split_once(')') else {
            break;
        };
        if count.is_empty() || !count.chars().all(|character| character.is_ascii_digit()) {
            break;
        }
        trimmed = suffix.trim_start();
    }

    trimmed
}

fn normalized_title_segment(segment: &str) -> String {
    strip_leading_notification_badges(segment).to_ascii_lowercase()
}

fn normalized_title_segments(name: &str) -> Vec<String> {
    name.split('|')
        .map(normalized_title_segment)
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn is_meeting_title_allowed(name: &str) -> bool {
    let segments = normalized_title_segments(name);
    if segments.is_empty() {
        return false;
    }

    if segments.len() == 1 && segments[0] == "microsoft teams" {
        return false;
    }

    !segments
        .iter()
        .any(|segment| NON_MEETING_TITLE_PREFIXES.contains(&segment.as_str()))
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
    use tokio::io::AsyncWriteExt;
    use tokio::sync::broadcast::error::TryRecvError;

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
    fn rejects_class_only_windows_without_teams_in_title() {
        let clients = vec![client("0xabc", "teams-for-linux", "Daily Sync")];

        assert_eq!(select_meeting_candidate(&clients), None);
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
        assert!(!is_meeting_title_allowed("(1) Calendar | Brent Wallace"));
        assert!(!is_meeting_title_allowed("(1) Activity | Brent Wallace"));
        assert!(!is_meeting_title_allowed("(1) Chat | Justin Gilmour"));
        assert!(!is_meeting_title_allowed(
            "(1) Chat | Meeting compact view | Krishna Kongara"
        ));
        assert!(!is_meeting_title_allowed("Waiting for network..."));
        assert!(!is_meeting_title_allowed("Teams and Channels"));
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

    #[test]
    fn spec_017_release_contract_covers_detection_state_transitions() {
        let bus = rustle_core::EventBus::new();
        let event_tx = bus.sender();
        let mut observer = bus.subscribe();
        let mut active_meeting = None;

        apply_detected_candidate(
            &event_tx,
            &mut active_meeting,
            Some(MeetingCandidate {
                address: "0xabc".to_owned(),
                name: "Roadmap Review".to_owned(),
                score: 2,
            }),
        );

        let started = observer.try_recv().expect("meeting start should publish");
        let started_id = match started {
            AppEvent::MeetingStarted { id, name, source } => {
                assert_eq!(name, "Roadmap Review");
                assert_eq!(source, DetectionSource::Hyprland);
                id
            }
            other => panic!("expected MeetingStarted event, got {other:?}"),
        };

        apply_detected_candidate(
            &event_tx,
            &mut active_meeting,
            Some(MeetingCandidate {
                address: "0xabc".to_owned(),
                name: "Roadmap Review (Renamed)".to_owned(),
                score: 2,
            }),
        );

        assert_eq!(
            active_meeting.as_ref().map(|meeting| meeting.name.as_str()),
            Some("Roadmap Review (Renamed)")
        );
        assert!(matches!(observer.try_recv(), Err(TryRecvError::Empty)));

        apply_detected_candidate(&event_tx, &mut active_meeting, None);

        let ended = observer.try_recv().expect("meeting end should publish");
        assert!(matches!(
            ended,
            AppEvent::MeetingEnded { id } if id == started_id
        ));
        assert!(active_meeting.is_none());
    }

    #[tokio::test]
    async fn spec_017_relevant_socket_events_trigger_candidate_refreshes() {
        let (stream, mut writer) = UnixStream::pair().expect("socket pair should open");
        let (trigger_tx, mut trigger_rx) = mpsc::unbounded_channel();

        let reader = tokio::spawn(async move { read_hyprland_events(stream, &trigger_tx).await });

        writer
            .write_all(
                b"workspace>>1\nwindowtitle>>Planning Sync\nactivewindowv2>>0xabc\nclosewindow>>0xabc\n",
            )
            .await
            .expect("socket payload should write");
        writer.shutdown().await.expect("socket should close");

        assert!(
            !reader.await.expect("socket reader should join"),
            "socket EOF should fall back to polling"
        );
        assert!(trigger_rx.recv().await.is_some());
        assert!(trigger_rx.recv().await.is_some());
        assert!(trigger_rx.recv().await.is_some());
        assert!(trigger_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn spec_017_socket_close_keeps_polling_available() {
        let (stream, mut writer) = UnixStream::pair().expect("socket pair should open");
        let (trigger_tx, mut trigger_rx) = mpsc::unbounded_channel();

        let reader = tokio::spawn(async move { read_hyprland_events(stream, &trigger_tx).await });

        writer
            .write_all(b"windowtitlev2>>Sprint Planning\n")
            .await
            .expect("socket payload should write");
        writer.shutdown().await.expect("socket should close");

        assert!(trigger_rx.recv().await.is_some());
        assert!(
            !reader.await.expect("socket reader should join"),
            "closed socket should keep the listener in polling mode"
        );
    }

    #[test]
    fn chat_window_without_teams_title_is_not_meeting() {
        let clients = vec![client("0xabc", "teams-for-linux", "Chat | Brent Wallace")];

        assert_eq!(select_meeting_candidate(&clients), None);
    }

    #[test]
    fn chat_window_with_teams_suffix_in_title_is_not_meeting() {
        let clients = vec![client(
            "0xabc",
            "teams-for-linux",
            "Chat | Brent Wallace | Microsoft Teams",
        )];

        assert_eq!(select_meeting_candidate(&clients), None);
    }

    #[test]
    fn chat_segment_after_contact_name_is_not_meeting() {
        let clients = vec![client(
            "0xabc",
            "teams-for-linux",
            "Brent Wallace | Chat | Microsoft Teams",
        )];

        assert_eq!(select_meeting_candidate(&clients), None);
        assert!(!is_meeting_title_allowed("Brent Wallace | Chat"));
    }

    #[test]
    fn real_meeting_window_with_teams_in_title_is_detected() {
        let clients = vec![client(
            "0xabc",
            "teams-for-linux",
            "Sprint Planning | Microsoft Teams",
        )];

        let candidate = select_meeting_candidate(&clients).expect("meeting candidate should exist");

        assert_eq!(candidate.name, "Sprint Planning");
        assert_eq!(candidate.score, 2);
    }

    #[test]
    fn browser_teams_meeting_is_detected() {
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
    fn teams_desktop_window_with_plain_chat_title_is_rejected() {
        let clients = vec![client("0xabc", "teams-for-linux", "Brent Wallace")];

        assert_eq!(select_meeting_candidate(&clients), None);
    }

    /// Spec 022: A Teams chat window must NOT trigger meeting start events,
    /// while a real Teams meeting window DOES trigger meeting start events.
    /// This test exercises the full detection flow end-to-end.
    #[test]
    fn spec_022_chat_window_does_not_trigger_meeting_flow() {
        let bus = rustle_core::EventBus::new();
        let event_tx = bus.sender();
        let mut observer = bus.subscribe();
        let mut active_meeting: Option<DetectedMeeting> = None;

        // --- Chat window must NOT trigger MeetingStarted ---
        let chat_clients = vec![
            client("0x111", "firefox", "GitHub"),
            client("0x222", "teams-for-linux", "Chat | User Name"),
        ];
        let chat_candidate = select_meeting_candidate(&chat_clients);
        assert!(
            chat_candidate.is_none(),
            "chat window should not produce a meeting candidate"
        );

        // Feed None into the state machine — should remain idle, no events.
        apply_detected_candidate(&event_tx, &mut active_meeting, chat_candidate);
        assert!(
            active_meeting.is_none(),
            "no meeting should be active after chat window"
        );
        assert!(
            matches!(observer.try_recv(), Err(TryRecvError::Empty)),
            "chat window must not emit any events"
        );

        // Also verify with the full Teams suffix on the chat title.
        let chat_with_suffix = vec![client(
            "0x333",
            "teams-for-linux",
            "Chat | User Name | Microsoft Teams",
        )];
        let chat_suffix_candidate = select_meeting_candidate(&chat_with_suffix);
        assert!(
            chat_suffix_candidate.is_none(),
            "chat window with Teams suffix should not produce a meeting candidate"
        );
        apply_detected_candidate(&event_tx, &mut active_meeting, chat_suffix_candidate);
        assert!(active_meeting.is_none());
        assert!(
            matches!(observer.try_recv(), Err(TryRecvError::Empty)),
            "chat window with suffix must not emit any events"
        );

        // --- Real meeting window DOES trigger MeetingStarted ---
        let meeting_clients = vec![
            client("0x444", "firefox", "GitHub"),
            client(
                "0x555",
                "teams-for-linux",
                "Sprint Planning | Microsoft Teams",
            ),
        ];
        let meeting_candidate = select_meeting_candidate(&meeting_clients);
        assert!(
            meeting_candidate.is_some(),
            "real meeting window should produce a meeting candidate"
        );

        let candidate = meeting_candidate.unwrap();
        assert_eq!(candidate.address, "0x555");
        assert_eq!(candidate.name, "Sprint Planning");
        assert_eq!(candidate.score, 2);

        // Apply the candidate — MeetingStarted must be emitted.
        apply_detected_candidate(&event_tx, &mut active_meeting, Some(candidate));
        assert!(
            active_meeting.is_some(),
            "meeting should be active after real meeting window"
        );

        let event = observer
            .try_recv()
            .expect("MeetingStarted event should be emitted");
        match event {
            AppEvent::MeetingStarted { id, name, source } => {
                assert_eq!(name, "Sprint Planning");
                assert_eq!(source, DetectionSource::Hyprland);
                assert_eq!(active_meeting.as_ref().unwrap().id, id);
            }
            other => panic!("expected MeetingStarted event, got {other:?}"),
        }

        // No further events should be pending.
        assert!(
            matches!(observer.try_recv(), Err(TryRecvError::Empty)),
            "only one MeetingStarted event should have been emitted"
        );
    }

    #[test]
    fn meeting_name_with_special_characters_is_normalized() {
        let result = normalize_meeting_name("Q4 Planning (Final) | Microsoft Teams");
        assert_eq!(result, Some("Q4 Planning (Final)".to_owned()));
    }

    #[test]
    fn empty_title_returns_none() {
        let c = client("0xabc", "teams-for-linux", "");
        assert_eq!(meeting_candidate_from_client(&c), None);
    }

    #[test]
    fn whitespace_only_title_returns_none() {
        let c = client("0xabc", "teams-for-linux", "   ");
        assert_eq!(meeting_candidate_from_client(&c), None);
    }

    #[test]
    fn multiple_meeting_windows_picks_highest_score() {
        let clients = vec![
            client(
                "0x111",
                "chromium",
                "Sprint Planning | Microsoft Teams | Google Chrome",
            ),
            client(
                "0x222",
                "teams-for-linux",
                "Daily Standup | Microsoft Teams",
            ),
        ];

        let candidate = select_meeting_candidate(&clients).expect("meeting candidate should exist");

        assert_eq!(candidate.score, 2);
        assert!(
            candidate.name == "Sprint Planning" || candidate.name == "Daily Standup",
            "expected one of the meeting names, got {:?}",
            candidate.name
        );
    }

    #[test]
    fn strips_notification_badges_from_meeting_names() {
        assert_eq!(
            normalize_meeting_name("(1) Sprint Planning | Microsoft Teams"),
            Some("Sprint Planning".to_owned())
        );
        assert_eq!(
            normalize_meeting_name("(12) Design Review | Weekly Sync | Microsoft Teams"),
            Some("Design Review | Weekly Sync".to_owned())
        );
    }

    #[test]
    fn log_derived_false_positive_titles_are_rejected() {
        for title in [
            "Waiting for network... | Microsoft Teams",
            "Teams and Channels | Microsoft Teams",
            "(1) Calendar | Sydney Crew | Microsoft Teams",
            "(1) Activity | Brent Wallace | Microsoft Teams",
            "(1) Chat | Justin Gilmour | Microsoft Teams",
            "(1) Chat | Meeting compact view | Krishna Kongara | Microsoft Teams",
        ] {
            let clients = vec![client("0xabc", "teams-for-linux", title)];
            assert_eq!(
                select_meeting_candidate(&clients),
                None,
                "{title} should not produce a meeting candidate"
            );
        }
    }

    #[test]
    fn unread_count_prefix_still_allows_real_meeting_detection() {
        let clients = vec![client(
            "0xabc",
            "teams-for-linux",
            "(1) Sprint Planning | Microsoft Teams",
        )];

        let candidate = select_meeting_candidate(&clients).expect("meeting candidate should exist");
        assert_eq!(candidate.name, "Sprint Planning");
        assert_eq!(candidate.score, 2);
    }

    #[test]
    fn log_derived_false_positive_titles_do_not_trigger_meeting_flow() {
        let bus = rustle_core::EventBus::new();
        let event_tx = bus.sender();
        let mut observer = bus.subscribe();
        let mut active_meeting: Option<DetectedMeeting> = None;

        for title in [
            "Waiting for network... | Microsoft Teams",
            "Teams and Channels | Microsoft Teams",
            "(1) Calendar | Sydney Crew | Microsoft Teams",
            "(1) Activity | Brent Wallace | Microsoft Teams",
            "(1) Chat | Justin Gilmour | Microsoft Teams",
            "(1) Chat | Meeting compact view | Krishna Kongara | Microsoft Teams",
        ] {
            let candidate = select_meeting_candidate(&[client("0xabc", "teams-for-linux", title)]);
            apply_detected_candidate(&event_tx, &mut active_meeting, candidate);
            assert!(
                active_meeting.is_none(),
                "{title} should not activate a meeting"
            );
            assert!(
                matches!(observer.try_recv(), Err(TryRecvError::Empty)),
                "{title} should not emit MeetingStarted"
            );
        }
    }
}
