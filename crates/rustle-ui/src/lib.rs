//! User interface crate for Rustle.

use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;

use rustle_core::{
    default_config_path, resolve_notes_dir, tasks::spawn_logged, AppEvent, CoreError,
    EventReceiver, EventSender, Settings, StoredDocumentKind,
};
use tokio::process::Command;
use tracing::{info, warn};

/// Initialises the user interface component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    drop(event_tx);

    if let Some(receiver) = event_rx {
        spawn_logged("notes-open-loop", notes_open_loop(receiver));
    }

    Ok(())
}

async fn notes_open_loop(mut event_rx: EventReceiver) {
    let mut meeting_active = false;
    let mut latest_note: Option<PathBuf> = None;
    let mut latest_transcript: Option<PathBuf> = None;

    loop {
        match event_rx.recv().await {
            Ok(AppEvent::MeetingStarted { .. }) => {
                meeting_active = true;
            }
            Ok(AppEvent::MeetingEnded { .. }) => {
                meeting_active = false;
            }
            Ok(AppEvent::NoteSaved { path, .. }) => {
                latest_note = Some(path);
            }
            Ok(AppEvent::TranscriptDraftReady { path, .. }) => {
                latest_transcript = Some(path);
            }
            Ok(AppEvent::DocumentDeleted { path, kind }) => match kind {
                StoredDocumentKind::Note => {
                    if latest_note.as_ref().is_some_and(|current| current == &path) {
                        latest_note = None;
                    }
                }
                StoredDocumentKind::Transcript => {
                    if latest_transcript
                        .as_ref()
                        .is_some_and(|current| current == &path)
                    {
                        latest_transcript = None;
                    }
                }
            },
            Ok(AppEvent::OpenNotesRequested) => {
                let notes_dir = configured_notes_dir().await;
                let (path, prefer_editor, purpose) = select_notes_open_target(
                    meeting_active,
                    latest_transcript.as_ref(),
                    latest_note.as_ref(),
                    &notes_dir,
                );
                open_path(&path, prefer_editor, purpose).await;
            }
            Ok(AppEvent::OpenTranscriptRequested) => {
                if let Some(path) = latest_transcript.clone() {
                    open_path(&path, true, "transcript draft").await;
                } else {
                    warn!("no transcript draft is available to open");
                }
            }
            Ok(AppEvent::OpenSettingsRequested) => {
                if let Some(path) = ensure_settings_file().await {
                    open_path(&path, true, "settings file").await;
                }
            }
            Ok(AppEvent::OpenPathRequested {
                path,
                prefer_editor,
            }) => {
                open_path(&path, prefer_editor, "requested path").await;
            }
            Ok(AppEvent::ShutdownComplete) => break,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "notes UI loop lagged behind event bus");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn ensure_settings_file() -> Option<PathBuf> {
    let path = match default_config_path() {
        Ok(path) => path,
        Err(error) => {
            warn!(%error, "failed to resolve settings path");
            return None;
        }
    };

    ensure_settings_file_at(path).await
}

async fn ensure_settings_file_at(path: PathBuf) -> Option<PathBuf> {
    match tokio::fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => Some(path),
        Ok(_) => {
            warn!(
                path = %path.display(),
                "settings path exists but is not a regular file"
            );
            None
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match Settings::default().save_to_path(&path).await {
                Ok(()) => Some(path),
                Err(error) => {
                    warn!(%error, path = %path.display(), "failed to create settings file");
                    None
                }
            }
        }
        Err(error) => {
            warn!(%error, path = %path.display(), "failed to inspect settings file");
            None
        }
    }
}

async fn configured_notes_dir() -> PathBuf {
    let settings = match Settings::load().await {
        Ok(settings) => settings,
        Err(error) => {
            warn!(%error, "failed to load settings for opening notes; using defaults");
            Settings::default().validated()
        }
    };

    resolve_notes_dir(&settings).unwrap_or_else(|_| PathBuf::from("."))
}

fn select_notes_open_target(
    meeting_active: bool,
    latest_transcript: Option<&PathBuf>,
    latest_note: Option<&PathBuf>,
    notes_dir: &Path,
) -> (PathBuf, bool, &'static str) {
    if meeting_active {
        if let Some(path) = latest_transcript {
            return (path.clone(), true, "current transcript draft");
        }
    }

    if let Some(path) = latest_note {
        return (path.clone(), true, "latest note");
    }

    (notes_dir.to_path_buf(), false, "notes folder")
}

async fn open_path(path: &Path, prefer_editor: bool, purpose: &'static str) {
    match launch_path(path, prefer_editor).await {
        Ok(()) => info!(path = %path.display(), prefer_editor, purpose, "opened path"),
        Err(error) => {
            warn!(%error, path = %path.display(), prefer_editor, purpose, "failed to open path")
        }
    }
}

async fn launch_path(path: &Path, prefer_editor: bool) -> std::io::Result<()> {
    let candidates = open_command_candidates(path, prefer_editor)?;
    let mut last_error = None;

    for command_line in candidates {
        match spawn_command(command_line).await {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        std::io::Error::other(format!("no opener is available for {}", path.display()))
    }))
}

fn open_command_candidates(path: &Path, prefer_editor: bool) -> std::io::Result<Vec<CommandLine>> {
    let mut commands = vec![CommandLine {
        program: OsString::from("xdg-open"),
        args: vec![path.as_os_str().to_owned()],
    }];

    if prefer_editor {
        if let Some(command) = preferred_editor_command(path)? {
            commands.push(command);
        }
    }

    Ok(commands)
}

fn preferred_editor_command(path: &Path) -> std::io::Result<Option<CommandLine>> {
    for variable in ["VISUAL", "EDITOR"] {
        let Some(value) = std::env::var_os(variable) else {
            continue;
        };

        let command = value.to_string_lossy().trim().to_owned();
        if command.is_empty() {
            continue;
        }

        let Some(mut parts) = shlex::split(&command) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("failed to parse {variable}={command}"),
            ));
        };
        if parts.is_empty() {
            continue;
        }

        let program = OsString::from(parts.remove(0));
        let mut args = parts.into_iter().map(OsString::from).collect::<Vec<_>>();
        args.push(path.as_os_str().to_owned());
        return Ok(Some(CommandLine { program, args }));
    }

    Ok(None)
}

async fn spawn_command(command_line: CommandLine) -> std::io::Result<()> {
    let mut command = Command::new(&command_line.program);
    command.args(&command_line.args);
    // Login services can start before the compositor imports its display environment.
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        if let Ok(Ok(output)) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            Command::new("systemctl")
                .args(["--user", "show-environment", "--output=json"])
                .kill_on_drop(true)
                .output(),
        )
        .await
        {
            if output.status.success() {
                command.envs(desktop_environment(&output.stdout));
            }
        }
    }
    let status = command.spawn()?.wait().await?;

    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{} exited with status {status}",
            command_line.program.to_string_lossy()
        )))
    }
}

fn desktop_environment(environment: &[u8]) -> Vec<(String, String)> {
    let Ok(environment) =
        serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(environment)
    else {
        return Vec::new();
    };
    environment
        .into_iter()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "DISPLAY"
                    | "WAYLAND_DISPLAY"
                    | "XAUTHORITY"
                    | "XDG_CURRENT_DESKTOP"
                    | "XDG_SESSION_TYPE"
            )
        })
        .filter_map(|(key, value)| value.as_str().map(|value| (key, value.to_owned())))
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
struct CommandLine {
    program: OsString,
    args: Vec<OsString>,
}

#[cfg(test)]
mod tests {
    use super::{
        desktop_environment, ensure_settings_file_at, open_command_candidates,
        preferred_editor_command, select_notes_open_target, spawn_command, CommandLine,
    };
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};
    static EDITOR_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn imports_only_desktop_variables_without_shell_evaluation() {
        assert_eq!(
            desktop_environment(br#"{"DISPLAY":":0","WAYLAND_DISPLAY":"wayland-1","XAUTHORITY":"/tmp/auth=a b","LD_PRELOAD":"/tmp/unsafe.so"}"#),
            vec![
                ("DISPLAY".to_owned(), ":0".to_owned()),
                ("WAYLAND_DISPLAY".to_owned(), "wayland-1".to_owned()),
                ("XAUTHORITY".to_owned(), "/tmp/auth=a b".to_owned()),
            ]
        );
        assert!(desktop_environment(b"not json").is_empty());
    }

    #[test]
    fn opener_recovers_display_environment_for_early_login_service() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = EDITOR_ENV.lock().unwrap();
        let temp_root = unique_temp_path("rustle-ui-session-test");
        std::fs::create_dir_all(&temp_root).unwrap();
        let manager = temp_root.join("systemctl");
        std::fs::write(
            &manager,
            "#!/bin/sh\nprintf '%s' '{\"DISPLAY\":\":88\",\"WAYLAND_DISPLAY\":\"wayland-test\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&manager, std::fs::Permissions::from_mode(0o700)).unwrap();
        let original =
            ["DISPLAY", "WAYLAND_DISPLAY", "PATH"].map(|key| (key, std::env::var_os(key)));
        std::env::remove_var("DISPLAY");
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::set_var("PATH", &temp_root);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(spawn_command(CommandLine {
            program: OsString::from("/bin/sh"),
            args: vec![
                OsString::from("-c"),
                OsString::from(
                    "test \"$DISPLAY\" = :88 && test \"$WAYLAND_DISPLAY\" = wayland-test",
                ),
            ],
        }));
        for (key, value) in original {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let _ = std::fs::remove_dir_all(temp_root);
        result.unwrap();
    }

    fn unique_temp_path(prefix: &str) -> std::path::PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("{prefix}-{timestamp}"))
    }

    #[tokio::test]
    async fn spec_027_missing_settings_file_is_created_before_open() {
        let temp_root = unique_temp_path("rustle-ui-test");
        let path = temp_root.join("config.toml");

        let created = ensure_settings_file_at(path.clone()).await;

        assert_eq!(created, Some(path.clone()));
        assert!(tokio::fs::metadata(&path).await.unwrap().is_file());
        let _ = tokio::fs::remove_dir_all(&temp_root).await;
    }

    #[tokio::test]
    async fn spec_027_settings_path_must_be_regular_file() {
        let temp_root = unique_temp_path("rustle-ui-test");
        tokio::fs::create_dir_all(&temp_root).await.unwrap();

        let resolved = ensure_settings_file_at(temp_root.clone()).await;

        assert_eq!(resolved, None);
        let _ = tokio::fs::remove_dir_all(&temp_root).await;
    }

    #[test]
    fn spec_027_visual_takes_precedence_over_editor_fallback() {
        let _guard = EDITOR_ENV.lock().unwrap();
        let original_visual = std::env::var_os("VISUAL");
        let original_editor = std::env::var_os("EDITOR");
        std::env::set_var("VISUAL", "nvim -f");
        std::env::set_var("EDITOR", "nano");

        let command = preferred_editor_command(Path::new("/tmp/test.md"))
            .expect("editor command should parse")
            .expect("editor command should exist");

        assert_eq!(
            command,
            CommandLine {
                program: OsString::from("nvim"),
                args: vec![OsString::from("-f"), OsString::from("/tmp/test.md")],
            }
        );

        match original_visual {
            Some(value) => std::env::set_var("VISUAL", value),
            None => std::env::remove_var("VISUAL"),
        }
        match original_editor {
            Some(value) => std::env::set_var("EDITOR", value),
            None => std::env::remove_var("EDITOR"),
        }
    }

    #[test]
    fn spec_027_xdg_open_is_tried_before_editor_fallback() {
        let _guard = EDITOR_ENV.lock().unwrap();
        let original_visual = std::env::var_os("VISUAL");
        let original_editor = std::env::var_os("EDITOR");
        std::env::set_var("VISUAL", "nvim -f");
        std::env::set_var("EDITOR", "nano");

        let commands =
            open_command_candidates(Path::new("/tmp/test.md"), true).expect("commands exist");

        assert_eq!(
            commands,
            vec![
                CommandLine {
                    program: OsString::from("xdg-open"),
                    args: vec![OsString::from("/tmp/test.md")],
                },
                CommandLine {
                    program: OsString::from("nvim"),
                    args: vec![OsString::from("-f"), OsString::from("/tmp/test.md")],
                },
            ]
        );

        match original_visual {
            Some(value) => std::env::set_var("VISUAL", value),
            None => std::env::remove_var("VISUAL"),
        }
        match original_editor {
            Some(value) => std::env::set_var("EDITOR", value),
            None => std::env::remove_var("EDITOR"),
        }
    }

    #[test]
    fn spec_025_open_routes_to_current_transcript_during_active_meeting() {
        let transcript = PathBuf::from("/tmp/current-transcript.txt");
        let note = PathBuf::from("/tmp/latest-note.md");
        let notes_dir = Path::new("/tmp/notes");

        let target = select_notes_open_target(true, Some(&transcript), Some(&note), notes_dir);

        assert_eq!(target, (transcript, true, "current transcript draft"));
    }

    #[test]
    fn spec_025_open_routes_to_latest_note_when_meeting_is_idle() {
        let transcript = PathBuf::from("/tmp/current-transcript.txt");
        let note = PathBuf::from("/tmp/latest-note.md");
        let notes_dir = Path::new("/tmp/notes");

        let target = select_notes_open_target(false, Some(&transcript), Some(&note), notes_dir);

        assert_eq!(target, (note, true, "latest note"));
    }

    #[test]
    fn spec_025_open_falls_back_to_notes_folder_without_documents() {
        let notes_dir = Path::new("/tmp/notes");

        let target = select_notes_open_target(false, None, None, notes_dir);

        assert_eq!(target, (PathBuf::from("/tmp/notes"), false, "notes folder"));
    }
}
