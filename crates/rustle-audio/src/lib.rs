//! Audio capture crate for Rustle.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustle_core::{
    default_data_dir, safe_filename, tasks::spawn_logged, AppEvent, CoreError, EventReceiver,
    EventSender, Settings,
};
use tokio::fs;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

const RECORDING_SAMPLE_RATE: &str = "16000";
const RECORDING_CHANNELS: &str = "1";
const STOP_POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAX_CHUNK_DURATION_SECS: u64 = 24 * 60 * 60;
const MAX_CHUNK_BYTES: u64 = 2 * 1024 * 1024 * 1024 * 1024;
const TEST_AUDIO_FILE_ENV: &str = "RUSTLE_TEST_AUDIO_FILE";

struct ActiveRecording {
    meeting_id: Uuid,
    stop_tx: mpsc::Sender<()>,
}

struct CaptureConfig {
    meeting_id: Uuid,
    meeting_name: String,
    input_device: String,
    chunk_duration: Duration,
    max_chunk_bytes: u64,
    event_tx: EventSender,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecorderBackend {
    PipeWire,
    PulseAudio,
    Alsa,
}

struct RecorderCommand {
    program: &'static str,
    args: Vec<String>,
}

/// Initialises the audio capture component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    if let Some(receiver) = event_rx {
        spawn_logged("audio-capture-loop", audio_capture_loop(event_tx, receiver));
    }

    Ok(())
}

async fn audio_capture_loop(event_tx: EventSender, mut event_rx: EventReceiver) {
    let mut active: Option<ActiveRecording> = None;

    loop {
        match event_rx.recv().await {
            Ok(AppEvent::MeetingStarted { id, name, .. }) => {
                if active.is_some() {
                    warn!("audio capture already has an active meeting");
                    continue;
                }
                if test_audio_file_configured() {
                    info!(meeting = %name, env = TEST_AUDIO_FILE_ENV, "test audio fixture configured; skipping live audio capture");
                    continue;
                }

                let settings = load_settings("audio capture").await;
                if !settings.meeting.auto_capture {
                    info!(meeting = %name, "auto audio capture disabled");
                    continue;
                }

                active = Some(start_recording(id, name, settings, event_tx.clone()));
            }
            Ok(AppEvent::MeetingEnded { id }) => {
                if let Some(recording) = active.take() {
                    if recording.meeting_id == id {
                        stop_recording(recording).await;
                    } else {
                        warn!(meeting_id = %id, active_id = %recording.meeting_id, "meeting ended while another recording was active");
                        active = Some(recording);
                    }
                }
            }
            Ok(AppEvent::QuitRequested) => {
                if let Some(recording) = active.take() {
                    stop_recording(recording).await;
                }
                break;
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "audio capture loop lagged behind event bus");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

fn start_recording(
    meeting_id: Uuid,
    meeting_name: String,
    settings: Settings,
    event_tx: EventSender,
) -> ActiveRecording {
    let (stop_tx, stop_rx) = mpsc::channel(1);
    let config = CaptureConfig {
        meeting_id,
        meeting_name,
        input_device: settings.audio.input_device,
        chunk_duration: chunk_duration(settings.audio.chunk_duration_minutes),
        max_chunk_bytes: recording_size_bytes(settings.audio.max_recording_size_mb),
        event_tx,
    };

    spawn_logged("audio-recording-worker", record_audio_loop(config, stop_rx));

    ActiveRecording {
        meeting_id,
        stop_tx,
    }
}

async fn stop_recording(recording: ActiveRecording) {
    if recording.stop_tx.send(()).await.is_err() {
        warn!(meeting_id = %recording.meeting_id, "audio capture worker already stopped");
    }
}

async fn record_audio_loop(config: CaptureConfig, mut stop_rx: mpsc::Receiver<()>) {
    let backend = match select_recorder_backend() {
        Some(backend) => backend,
        None => {
            warn!("no supported Linux audio recorder found; install PipeWire, PulseAudio, or ALSA recording tools");
            return;
        }
    };
    let directory = match default_data_dir() {
        Ok(path) => path.join("recordings"),
        Err(error) => {
            warn!(%error, "failed to resolve recordings directory");
            return;
        }
    };
    if let Err(error) = fs::create_dir_all(&directory).await {
        warn!(%error, path = %directory.display(), "failed to create recordings directory");
        return;
    }

    publish(
        &config.event_tx,
        AppEvent::RecordingStarted {
            meeting_id: config.meeting_id,
        },
    );
    info!(
        meeting_id = %config.meeting_id,
        backend = ?backend,
        "audio recording started"
    );

    let mut chunk_index = 1_u64;
    loop {
        let path = chunk_path(&directory, &config.meeting_name, chunk_index);
        let stopped = match record_chunk(&config, backend, &path, &mut stop_rx).await {
            Ok(stopped) => stopped,
            Err(error) => {
                warn!(%error, path = %path.display(), "failed to record audio chunk");
                true
            }
        };

        if file_has_audio(&path).await {
            publish(
                &config.event_tx,
                AppEvent::RecordingChunkReady {
                    meeting_id: config.meeting_id,
                    path,
                },
            );
        } else if let Err(error) = fs::remove_file(&path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(%error, path = %path.display(), "failed to remove empty recording chunk");
            }
        }

        if stopped {
            break;
        }
        chunk_index += 1;
    }

    publish(
        &config.event_tx,
        AppEvent::RecordingStopped {
            meeting_id: config.meeting_id,
        },
    );
    info!(meeting_id = %config.meeting_id, "audio recording stopped");
}

async fn record_chunk(
    config: &CaptureConfig,
    backend: RecorderBackend,
    path: &Path,
    stop_rx: &mut mpsc::Receiver<()>,
) -> Result<bool, String> {
    let command = recorder_command(backend, &config.input_device, path);
    let mut child = Command::new(command.program)
        .args(command.args)
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("failed to launch recorder: {error}"))?;
    let mut interval = tokio::time::interval(STOP_POLL_INTERVAL);
    let duration = tokio::time::sleep(config.chunk_duration.max(STOP_POLL_INTERVAL));
    tokio::pin!(duration);

    loop {
        tokio::select! {
            status = child.wait() => {
                let status = status.map_err(|error| format!("failed to wait for recorder: {error}"))?;
                if !status.success() {
                    return Err(format!("recorder exited with status {status}"));
                }
                return Ok(true);
            }
            _ = stop_rx.recv() => {
                stop_child(&mut child).await;
                return Ok(true);
            }
            _ = &mut duration => {
                stop_child(&mut child).await;
                return Ok(false);
            }
            _ = interval.tick() => {
                if file_len(path).await >= config.max_chunk_bytes {
                    stop_child(&mut child).await;
                    return Ok(false);
                }
            }
        }
    }
}

async fn stop_child(child: &mut tokio::process::Child) {
    if let Err(error) = child.start_kill() {
        warn!(%error, "failed to stop audio recorder");
    }
    if let Err(error) = child.wait().await {
        warn!(%error, "failed to wait for stopped audio recorder");
    }
}

fn select_recorder_backend() -> Option<RecorderBackend> {
    [
        (RecorderBackend::PipeWire, "pw-record"),
        (RecorderBackend::PulseAudio, "parecord"),
        (RecorderBackend::Alsa, "arecord"),
    ]
    .into_iter()
    .find_map(|(backend, program)| executable_in_path(program).then_some(backend))
}

fn executable_in_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };

    std::env::split_paths(&paths)
        .map(|directory| directory.join(program))
        .any(|candidate| is_executable_file(&candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn recorder_command(backend: RecorderBackend, input_device: &str, path: &Path) -> RecorderCommand {
    let input_device = input_device.trim();
    let configured_device =
        !input_device.is_empty() && !input_device.eq_ignore_ascii_case("default");
    let path = path.display().to_string();

    match backend {
        RecorderBackend::PipeWire => {
            let mut args = vec![
                "--rate".to_owned(),
                RECORDING_SAMPLE_RATE.to_owned(),
                "--channels".to_owned(),
                RECORDING_CHANNELS.to_owned(),
                "--format".to_owned(),
                "s16".to_owned(),
            ];
            if configured_device {
                args.extend(["--target".to_owned(), input_device.to_owned()]);
            }
            args.push(path);
            RecorderCommand {
                program: "pw-record",
                args,
            }
        }
        RecorderBackend::PulseAudio => {
            let mut args = vec![
                "--file-format=wav".to_owned(),
                "--format=s16le".to_owned(),
                format!("--rate={RECORDING_SAMPLE_RATE}"),
                format!("--channels={RECORDING_CHANNELS}"),
            ];
            if configured_device {
                args.push(format!("--device={input_device}"));
            }
            args.push(path);
            RecorderCommand {
                program: "parecord",
                args,
            }
        }
        RecorderBackend::Alsa => {
            let mut args = vec![
                "-f".to_owned(),
                "S16_LE".to_owned(),
                "-r".to_owned(),
                RECORDING_SAMPLE_RATE.to_owned(),
                "-c".to_owned(),
                RECORDING_CHANNELS.to_owned(),
                "-t".to_owned(),
                "wav".to_owned(),
            ];
            if configured_device {
                args.extend(["-D".to_owned(), input_device.to_owned()]);
            }
            args.push(path);
            RecorderCommand {
                program: "arecord",
                args,
            }
        }
    }
}

fn chunk_path(directory: &Path, meeting_name: &str, index: u64) -> PathBuf {
    directory.join(format!(
        "{}_{}_chunk_{index:04}.wav",
        unix_timestamp_seconds(),
        safe_filename(meeting_name)
    ))
}

async fn file_has_audio(path: &Path) -> bool {
    file_len(path).await > 44
}

async fn file_len(path: &Path) -> u64 {
    fs::metadata(path)
        .await
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn chunk_duration(minutes: u64) -> Duration {
    Duration::from_secs(minutes.saturating_mul(60).clamp(1, MAX_CHUNK_DURATION_SECS))
}

fn recording_size_bytes(megabytes: u64) -> u64 {
    megabytes
        .saturating_mul(1024)
        .saturating_mul(1024)
        .clamp(1, MAX_CHUNK_BYTES)
}

fn test_audio_file_configured() -> bool {
    std::env::var(TEST_AUDIO_FILE_ENV)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
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

fn publish(event_tx: &EventSender, event: AppEvent) {
    if let Err(error) = event_tx.send(event) {
        warn!(%error, "failed to publish event");
    }
}

fn unix_timestamp_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_005_pipewire_command_uses_configured_target() {
        let command = recorder_command(
            RecorderBackend::PipeWire,
            "alsa_input.pci.capture",
            Path::new("/tmp/chunk.wav"),
        );

        assert_eq!(command.program, "pw-record");
        assert!(command.args.contains(&"--target".to_owned()));
        assert!(command.args.contains(&"alsa_input.pci.capture".to_owned()));
        assert_eq!(
            command.args.last().map(String::as_str),
            Some("/tmp/chunk.wav")
        );
    }

    #[test]
    fn spec_005_alsa_default_device_omits_explicit_device() {
        let command = recorder_command(RecorderBackend::Alsa, "default", Path::new("/tmp/a.wav"));

        assert_eq!(command.program, "arecord");
        assert!(!command.args.contains(&"-D".to_owned()));
    }

    #[test]
    fn spec_005_audio_settings_arithmetic_is_saturating_and_clamped() {
        assert_eq!(
            chunk_duration(u64::MAX),
            Duration::from_secs(MAX_CHUNK_DURATION_SECS)
        );
        assert_eq!(recording_size_bytes(u64::MAX), MAX_CHUNK_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn spec_005_recorder_detection_requires_executable_file() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "rustle-non-executable-recorder-{}",
            unix_timestamp_seconds()
        ));
        std::fs::write(&path, "").expect("test file should be created");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("test file permissions should be set");

        assert!(!is_executable_file(&path));

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("test file permissions should be made executable");
        assert!(is_executable_file(&path));

        std::fs::remove_file(path).expect("test file should be removed");
    }
}
