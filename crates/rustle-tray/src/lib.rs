//! System tray crate for Rustle.

use std::path::PathBuf;

use ksni::menu::{StandardItem, SubMenu};
use ksni::{Category, Handle, MenuItem, Status, ToolTip, Tray, TrayMethods};
use rustle_core::{
    default_data_dir, resolve_notes_dir, resolve_transcripts_dir, tasks::spawn_logged,
    AiProvider, AppEvent, CoreError, DetectionSource, EventReceiver, EventSender,
    RecordingDetectionMethod, Settings, StoredDocument, StoredDocumentKind, TranscriptionMethod,
};
use rustle_storage::{list_notes, list_transcripts};
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Clone)]
struct RustleTray {
    event_tx: EventSender,
    active_meeting: Option<(Uuid, String)>,
    settings: Settings,
    notes: Vec<StoredDocument>,
    transcripts: Vec<StoredDocument>,
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
    let settings = load_settings("tray startup").await;
    let (notes, transcripts) = load_documents(&settings).await;
    let tray = RustleTray {
        event_tx,
        active_meeting: None,
        settings: settings.clone(),
        notes,
        transcripts,
    };

    match tray.assume_sni_available(true).spawn().await {
        Ok(handle) => {
            info!("system tray registered");
            spawn_logged("tray-status-loop", status_loop(receiver, handle, settings));
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
        resolve_icon_theme_path()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
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
        if let Some(document) = self.notes.first() {
            self.open_path(document.path.clone(), true);
            return;
        }

        if let Some(path) = self.notes_dir() {
            self.open_path(path, false);
            return;
        }

        publish(&self.event_tx, AppEvent::OpenNotesRequested);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            notes_submenu(self),
            transcripts_submenu(self),
            current_meeting_item(self.active_meeting.as_ref()),
            MenuItem::Separator,
            meeting_submenu(self.active_meeting.is_some()),
            MenuItem::Separator,
            settings_submenu(self),
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

    fn open_path(&self, path: PathBuf, prefer_editor: bool) {
        publish(
            &self.event_tx,
            AppEvent::OpenPathRequested { path, prefer_editor },
        );
    }

    fn delete_document(&self, path: PathBuf, kind: StoredDocumentKind) {
        publish(
            &self.event_tx,
            AppEvent::DeleteDocumentRequested { path, kind },
        );
    }

    fn summarise_transcript(&self, path: PathBuf) {
        publish(
            &self.event_tx,
            AppEvent::SummariseTranscriptRequested { path },
        );
    }

    fn request_settings_update(
        &self,
        update: impl FnOnce(&mut Settings) + Send + 'static,
    ) {
        request_settings_update(self.event_tx.clone(), update);
    }

    fn notes_dir(&self) -> Option<PathBuf> {
        resolve_notes_dir(&self.settings).ok()
    }

    fn transcripts_dir(&self) -> Option<PathBuf> {
        resolve_transcripts_dir().ok()
    }
}

fn standard_item(
    label: impl Into<String>,
    enabled: bool,
    icon_name: impl Into<String>,
    activate: impl Fn(&mut RustleTray) + Send + 'static,
) -> MenuItem<RustleTray> {
    StandardItem {
        label: label.into(),
        enabled,
        icon_name: icon_name.into(),
        activate: Box::new(activate),
        ..StandardItem::default()
    }
    .into()
}

fn disabled_item(label: impl Into<String>, icon_name: impl Into<String>) -> MenuItem<RustleTray> {
    StandardItem {
        label: label.into(),
        enabled: false,
        icon_name: icon_name.into(),
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

fn notes_submenu(tray: &RustleTray) -> MenuItem<RustleTray> {
    let mut submenu = Vec::new();
    let latest_note = tray.notes.first().map(|document| document.path.clone());
    submenu.push(standard_item(
        "Open _Latest Note",
        latest_note.is_some(),
        "document-open",
        move |tray| {
            if let Some(path) = latest_note.clone() {
                tray.open_path(path, true);
            }
        },
    ));

    let notes_dir = tray.notes_dir();
    submenu.push(standard_item(
        "Open Notes _Folder",
        notes_dir.is_some(),
        "folder-open",
        move |tray| {
            if let Some(path) = notes_dir.clone() {
                tray.open_path(path, false);
            }
        },
    ));

    if tray.notes.is_empty() {
        submenu.push(disabled_item("No saved notes yet", "text-markdown"));
    } else {
        submenu.push(MenuItem::Separator);
        submenu.extend(tray.notes.iter().cloned().map(note_document_submenu));
    }

    SubMenu {
        label: format!("_Notes ({})", tray.notes.len()),
        icon_name: "text-markdown".to_owned(),
        submenu,
        ..SubMenu::default()
    }
    .into()
}

fn transcripts_submenu(tray: &RustleTray) -> MenuItem<RustleTray> {
    let mut submenu = Vec::new();
    let latest_transcript = tray.transcripts.first().map(|document| document.path.clone());
    submenu.push(standard_item(
        "Open Latest _Transcript",
        latest_transcript.is_some(),
        "text-x-generic",
        move |tray| {
            if let Some(path) = latest_transcript.clone() {
                tray.open_path(path, true);
            }
        },
    ));

    let transcript_dir = tray.transcripts_dir();
    submenu.push(standard_item(
        "Open Transcript _Folder",
        transcript_dir.is_some(),
        "folder-open",
        move |tray| {
            if let Some(path) = transcript_dir.clone() {
                tray.open_path(path, false);
            }
        },
    ));

    if tray.transcripts.is_empty() {
        submenu.push(disabled_item("No saved transcripts yet", "text-x-generic"));
    } else {
        submenu.push(MenuItem::Separator);
        submenu.extend(
            tray.transcripts
                .iter()
                .cloned()
                .map(transcript_document_submenu),
        );
    }

    SubMenu {
        label: format!("_Transcripts ({})", tray.transcripts.len()),
        icon_name: "text-x-generic".to_owned(),
        submenu,
        ..SubMenu::default()
    }
    .into()
}

fn note_document_submenu(document: StoredDocument) -> MenuItem<RustleTray> {
    let open_path = document.path.clone();
    let delete_path = document.path.clone();

    SubMenu {
        label: document_menu_label(&document),
        icon_name: "text-markdown".to_owned(),
        submenu: vec![
            standard_item("_Open", true, "document-open", move |tray| {
                tray.open_path(open_path.clone(), true);
            }),
            standard_item("_Delete Permanently", true, "user-trash", move |tray| {
                tray.delete_document(delete_path.clone(), StoredDocumentKind::Note);
            }),
        ],
        ..SubMenu::default()
    }
    .into()
}

fn transcript_document_submenu(document: StoredDocument) -> MenuItem<RustleTray> {
    let open_path = document.path.clone();
    let summarise_path = document.path.clone();
    let delete_path = document.path.clone();

    SubMenu {
        label: document_menu_label(&document),
        icon_name: "text-x-generic".to_owned(),
        submenu: vec![
            standard_item("_Open", true, "document-open", move |tray| {
                tray.open_path(open_path.clone(), true);
            }),
            standard_item("_Summarise To New Note", true, "document-save", move |tray| {
                tray.summarise_transcript(summarise_path.clone());
            }),
            standard_item("_Delete Permanently", true, "user-trash", move |tray| {
                tray.delete_document(delete_path.clone(), StoredDocumentKind::Transcript);
            }),
        ],
        ..SubMenu::default()
    }
    .into()
}

fn settings_submenu(tray: &RustleTray) -> MenuItem<RustleTray> {
    SubMenu {
        label: "_Settings".to_owned(),
        icon_name: "preferences-system".to_owned(),
        submenu: vec![
            bool_setting_submenu(
                "Auto Detect",
                tray.settings.meeting.auto_detect,
                "system-search",
                set_auto_detect,
            ),
            bool_setting_submenu(
                "Auto Capture",
                tray.settings.meeting.auto_capture,
                "media-record",
                set_auto_capture,
            ),
            bool_setting_submenu(
                "Capture Loopback",
                tray.settings.audio.capture_loopback,
                "audio-card",
                set_capture_loopback,
            ),
            detection_method_submenu(&tray.settings),
            transcription_method_submenu(&tray.settings),
            ai_provider_submenu(&tray.settings),
            MenuItem::Separator,
            standard_item("Open Config In _Editor", true, "preferences-desktop-text-to-speech", |tray| {
                publish(&tray.event_tx, AppEvent::OpenSettingsRequested);
            }),
        ],
        ..SubMenu::default()
    }
    .into()
}

fn bool_setting_submenu(
    label: &'static str,
    current: bool,
    icon_name: &'static str,
    setter: fn(&mut Settings, bool),
) -> MenuItem<RustleTray> {
    SubMenu {
        label: format!("{label}: {}", on_off(current)),
        icon_name: icon_name.to_owned(),
        submenu: vec![
            standard_item("Turn _On", !current, "object-select", move |tray| {
                tray.request_settings_update(move |settings| setter(settings, true));
            }),
            standard_item("Turn O_ff", current, "window-close", move |tray| {
                tray.request_settings_update(move |settings| setter(settings, false));
            }),
        ],
        ..SubMenu::default()
    }
    .into()
}

fn detection_method_submenu(settings: &Settings) -> MenuItem<RustleTray> {
    let current = settings.meeting.detection_method.clone();
    disabled_item(
        format!("Detection Method: {}", detection_method_label(&current)),
        "preferences-system-windows",
    )
}

fn transcription_method_submenu(settings: &Settings) -> MenuItem<RustleTray> {
    let current = settings.transcription.method.clone();
    SubMenu {
        label: format!("Transcription: {}", transcription_method_label(&current)),
        icon_name: "audio-x-generic".to_owned(),
        submenu: vec![
            choice_setting_item(
                "Local Whisper",
                matches!(current, TranscriptionMethod::Local),
                "computer",
                TranscriptionMethod::Local,
                set_transcription_method,
            ),
            choice_setting_item(
                "OpenAI Whisper",
                matches!(current, TranscriptionMethod::Openai),
                "network-server",
                TranscriptionMethod::Openai,
                set_transcription_method,
            ),
        ],
        ..SubMenu::default()
    }
    .into()
}

fn ai_provider_submenu(settings: &Settings) -> MenuItem<RustleTray> {
    let current = settings.ai.provider.clone();
    SubMenu {
        label: format!("AI Provider: {}", ai_provider_label(&current)),
        icon_name: "applications-science".to_owned(),
        submenu: vec![
            choice_setting_item(
                "llama.cpp",
                matches!(current, AiProvider::LlamaCpp),
                "computer",
                AiProvider::LlamaCpp,
                set_ai_provider,
            ),
            choice_setting_item(
                "OpenAI",
                matches!(current, AiProvider::Openai),
                "network-server",
                AiProvider::Openai,
                set_ai_provider,
            ),
            choice_setting_item(
                "Anthropic",
                matches!(current, AiProvider::Anthropic),
                "network-server",
                AiProvider::Anthropic,
                set_ai_provider,
            ),
            choice_setting_item(
                "Ollama",
                matches!(current, AiProvider::Ollama),
                "network-workgroup",
                AiProvider::Ollama,
                set_ai_provider,
            ),
        ],
        ..SubMenu::default()
    }
    .into()
}

fn choice_setting_item<T>(
    label: &'static str,
    selected: bool,
    icon_name: &'static str,
    value: T,
    setter: fn(&mut Settings, T),
) -> MenuItem<RustleTray>
where
    T: Clone + Send + 'static,
{
    standard_item(label, !selected, icon_name, move |tray| {
        let next_value = value.clone();
        tray.request_settings_update(move |settings| setter(settings, next_value));
    })
}

fn document_menu_label(document: &StoredDocument) -> String {
    document.title.clone()
}

fn on_off(value: bool) -> &'static str {
    if value { "On" } else { "Off" }
}

fn detection_method_label(method: &RecordingDetectionMethod) -> &'static str {
    match method {
        RecordingDetectionMethod::Hyprland => "Hyprland",
    }
}

fn transcription_method_label(method: &TranscriptionMethod) -> &'static str {
    match method {
        TranscriptionMethod::Local => "Local Whisper",
        TranscriptionMethod::Openai => "OpenAI Whisper",
    }
}

fn ai_provider_label(provider: &AiProvider) -> &'static str {
    match provider {
        AiProvider::LlamaCpp => "llama.cpp",
        AiProvider::Openai => "OpenAI",
        AiProvider::Anthropic => "Anthropic",
        AiProvider::Ollama => "Ollama",
    }
}

fn set_auto_detect(settings: &mut Settings, value: bool) {
    settings.meeting.auto_detect = value;
}

fn set_auto_capture(settings: &mut Settings, value: bool) {
    settings.meeting.auto_capture = value;
}

fn set_capture_loopback(settings: &mut Settings, value: bool) {
    settings.audio.capture_loopback = value;
}

fn set_transcription_method(settings: &mut Settings, value: TranscriptionMethod) {
    settings.transcription.method = value;
}

fn set_ai_provider(settings: &mut Settings, value: AiProvider) {
    settings.ai.provider = value;
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

async fn status_loop(
    mut event_rx: EventReceiver,
    handle: Handle<RustleTray>,
    mut settings: Settings,
) {
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
                refresh_documents(&handle, &settings).await;
            }
            Ok(AppEvent::NoteSaved { path, .. }) => {
                info!(path = %path.display(), "note saved");
                refresh_documents(&handle, &settings).await;
            }
            Ok(AppEvent::DocumentDeleted { path, kind }) => {
                info!(path = %path.display(), ?kind, "document deleted");
                refresh_documents(&handle, &settings).await;
            }
            Ok(AppEvent::SettingsChanged(updated_settings)) => {
                settings = updated_settings.validated();
                refresh_documents(&handle, &settings).await;
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

async fn refresh_documents(handle: &Handle<RustleTray>, settings: &Settings) {
    let (notes, transcripts) = load_documents(settings).await;
    let settings = settings.clone();
    update_tray(handle, move |tray| {
        tray.settings = settings;
        tray.notes = notes;
        tray.transcripts = transcripts;
    })
    .await;
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

fn request_settings_update(
    event_tx: EventSender,
    update: impl FnOnce(&mut Settings) + Send + 'static,
) {
    spawn_logged("tray-save-settings", async move {
        let mut settings = load_settings("tray settings update").await;
        update(&mut settings);
        let settings = settings.validated();

        match settings.save().await {
            Ok(()) => publish(&event_tx, AppEvent::SettingsChanged(settings)),
            Err(error) => warn!(%error, "failed to persist tray settings change"),
        }
    });
}

async fn load_settings(purpose: &'static str) -> Settings {
    match Settings::load().await {
        Ok(settings) => settings.validated(),
        Err(error) => {
            warn!(%error, purpose, "failed to load settings; using defaults");
            Settings::default().validated()
        }
    }
}

async fn load_documents(settings: &Settings) -> (Vec<StoredDocument>, Vec<StoredDocument>) {
    let notes = match list_notes(settings).await {
        Ok(documents) => documents,
        Err(error) => {
            warn!(%error, "failed to load note inventory for tray");
            Vec::new()
        }
    };

    let transcripts = match list_transcripts().await {
        Ok(documents) => documents,
        Err(error) => {
            warn!(%error, "failed to load transcript inventory for tray");
            Vec::new()
        }
    };

    (notes, transcripts)
}

fn unix_timestamp_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn resolve_icon_theme_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(path) = default_data_dir() {
        candidates.push(path.join("icons"));
    }

    let mut shared_data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .filter(|paths| !paths.is_empty())
        .unwrap_or_else(|| {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        });
    candidates.extend(
        shared_data_dirs
            .drain(..)
            .map(|path| path.join("rustle/icons")),
    );

    resolve_icon_theme_path_from_candidates(candidates)
}

fn resolve_icon_theme_path_from_candidates(
    candidates: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    candidates.into_iter().find(|path| path.is_dir())
}

#[cfg(test)]
mod tests {
    use super::{document_menu_label, resolve_icon_theme_path_from_candidates};
    use rustle_core::{StoredDocument, StoredDocumentKind};
    use std::path::PathBuf;

    #[test]
    fn uses_first_existing_icon_path_candidate() {
        let temp_root =
            std::env::temp_dir().join(format!("rustle-tray-test-{}", uuid::Uuid::new_v4()));
        let icon_dir = temp_root.join("rustle/icons");
        std::fs::create_dir_all(&icon_dir).unwrap();
        let missing_dir = temp_root.join("missing");
        let resolved = resolve_icon_theme_path_from_candidates([missing_dir, icon_dir.clone()]);

        assert_eq!(resolved, Some(icon_dir));
        let _ = std::fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn document_menu_label_uses_human_title() {
        let label = document_menu_label(&StoredDocument {
            kind: StoredDocumentKind::Note,
            path: PathBuf::from("/tmp/1779283974_team_sync.md"),
            title: "team sync".to_owned(),
            timestamp_seconds: 1_779_283_974,
        });

        assert_eq!(label, "team sync");
    }
}
