//! Audio capture crate for Rustle.

pub mod pipewire;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustle_core::{
    default_data_dir, safe_filename, tasks::spawn_logged, AppEvent, CoreError, EventReceiver,
    EventSender, Settings,
};
use tokio::fs;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

const RECORDING_SAMPLE_RATE: &str = "16000";
const RECORDING_CHANNELS: &str = "1";
const STOP_POLL_INTERVAL: Duration = Duration::from_secs(1);
const RECORDER_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const RECORDER_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CHUNK_DURATION_SECS: u64 = 24 * 60 * 60;
// Keep RIFF WAV chunks below the format's 32-bit data-size limit.
const MAX_CHUNK_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const TEST_AUDIO_FILE_ENV: &str = "RUSTLE_TEST_AUDIO_FILE";

struct ActiveRecording {
    meeting_id: Uuid,
    stop_tx: mpsc::Sender<()>,
    worker: tokio::task::JoinHandle<()>,
}

struct CaptureConfig {
    meeting_id: Uuid,
    meeting_name: String,
    input_device: String,
    output_device: String,
    capture_loopback: bool,
    recording_started: bool,
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
                if let Some(previous) = active.take() {
                    let previous_id = previous.meeting_id;
                    stop_recording(previous).await;
                    publish(&event_tx, AppEvent::MeetingEnded { id: previous_id });
                }
                if test_audio_file_configured() {
                    info!(meeting = %name, env = TEST_AUDIO_FILE_ENV, "test audio fixture configured; skipping live audio capture");
                    publish(&event_tx, AppEvent::RecordingStopped { meeting_id: id });
                    continue;
                }

                let settings = load_settings("audio capture").await;
                if !settings.meeting.auto_capture {
                    info!(meeting = %name, "auto audio capture disabled");
                    publish(&event_tx, AppEvent::RecordingStopped { meeting_id: id });
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
        output_device: settings.audio.output_device,
        capture_loopback: settings.audio.capture_loopback,
        recording_started: false,
        chunk_duration: chunk_duration(settings.audio.chunk_duration_minutes),
        max_chunk_bytes: recording_size_bytes(settings.audio.max_recording_size_mb),
        event_tx,
    };

    let worker = tokio::spawn(record_audio_loop(config, stop_rx));

    ActiveRecording {
        meeting_id,
        stop_tx,
        worker,
    }
}

async fn stop_recording(recording: ActiveRecording) {
    if recording.stop_tx.send(()).await.is_err() {
        warn!(meeting_id = %recording.meeting_id, "audio capture worker already stopped");
    }
    if let Err(error) = recording.worker.await {
        warn!(%error, "recording worker failed");
    }
}

async fn record_audio_loop(mut config: CaptureConfig, mut stop_rx: mpsc::Receiver<()>) {
    if let Err(error) = capture(&mut config, &mut stop_rx).await {
        warn!(%error, "audio capture failed; recording retained");
        publish(
            &config.event_tx,
            AppEvent::WorkflowFailed {
                meeting_id: config.meeting_id,
                stage: "recording".to_owned(),
                message: error,
            },
        );
    }
    publish(
        &config.event_tx,
        AppEvent::RecordingStopped {
            meeting_id: config.meeting_id,
        },
    );
}

async fn capture(
    config: &mut CaptureConfig,
    stop_rx: &mut mpsc::Receiver<()>,
) -> Result<(), String> {
    let backend = select_recorder_backend()
        .ok_or("install pipewire-utils (Fedora) or pipewire-audio (Arch)")?;
    if config.capture_loopback && backend != RecorderBackend::PipeWire {
        return Err(
            "microphone + output capture requires PipeWire; refusing microphone-only fallback"
                .to_owned(),
        );
    }
    let directory = default_data_dir()
        .map_err(|e| e.to_string())?
        .join("recordings");
    fs::create_dir_all(&directory)
        .await
        .map_err(|e| e.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|e| e.to_string())?;
    let mut graph = if config.capture_loopback {
        Some(pipewire::MixGraph::start(&config.input_device, &config.output_device).await?)
    } else {
        None
    };
    let target = graph
        .as_ref()
        .map(|g| g.source.clone())
        .unwrap_or_else(|| config.input_device.clone());
    let result = async {
        let mut chunk_index = 1;
        loop {
            let path = chunk_path(
                &directory,
                &format!("{}_{}", config.meeting_name, config.meeting_id),
                chunk_index,
            );
            let command = recorder_command(backend, &target, &path);
            let result = record_chunk(config, command, &path, stop_rx, &mut graph).await;
            if file_has_audio(&path).await {
                publish(
                    &config.event_tx,
                    AppEvent::RecordingChunkReady {
                        meeting_id: config.meeting_id,
                        path,
                    },
                );
            }
            if result? {
                return Ok(());
            }
            chunk_index += 1;
        }
    }
    .await;
    if let Some(graph) = graph {
        graph.stop().await;
    }
    result
}

async fn record_chunk(
    config: &mut CaptureConfig,
    command: RecorderCommand,
    path: &Path,
    stop_rx: &mut mpsc::Receiver<()>,
    graph: &mut Option<pipewire::MixGraph>,
) -> Result<bool, String> {
    let mut child = Command::new(command.program)
        .args(command.args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("failed to launch recorder: {error}"))?;
    let mut interval = tokio::time::interval(STOP_POLL_INTERVAL);
    let startup_timeout = tokio::time::sleep(RECORDER_STARTUP_TIMEOUT.min(config.chunk_duration));
    let duration = tokio::time::sleep(config.chunk_duration.max(STOP_POLL_INTERVAL));
    let mut startup_checked = false;
    tokio::pin!(startup_timeout);
    tokio::pin!(duration);
    loop {
        tokio::select! {
            status = child.wait() => {
                let status = status.map_err(|error| format!("failed to wait for recorder: {error}"))?;
                return Err(format!("recorder exited unexpectedly with status {status}; see recorder stderr"));
            }
            _ = stop_rx.recv() => {
                stop_child(&mut child).await?;
                return Ok(true);
            }
            _ = interval.tick() => {
                if let Some(graph) = graph {
                    if let Err(error) = graph.refresh(&config.input_device, &config.output_device).await {
                        stop_child(&mut child).await?;
                        return Err(error);
                    }
                }
                if !startup_checked && wav_data_present(path).await {
                    startup_checked = true;
                    announce_recording(config);
                }
                if file_len(path).await >= config.max_chunk_bytes {
                    stop_child(&mut child).await?;
                    return Ok(false);
                }
            }
            _ = &mut startup_timeout, if !startup_checked => {
                startup_checked = true;
                if !wav_data_present(path).await {
                    stop_child(&mut child).await?;
                    return Err(format!(
                        "recorder {} did not produce audio within {}s",
                        command.program,
                        RECORDER_STARTUP_TIMEOUT.as_secs()
                    ));
                }
                announce_recording(config);
            }
            _ = &mut duration => {
                stop_child(&mut child).await?;
                return Ok(false);
            }
        }
    }
}

fn announce_recording(config: &mut CaptureConfig) {
    if !config.recording_started {
        config.recording_started = true;
        publish(
            &config.event_tx,
            AppEvent::RecordingStarted {
                meeting_id: config.meeting_id,
            },
        );
        info!(meeting_id = %config.meeting_id, "audio recording started");
    }
}
async fn stop_child(child: &mut tokio::process::Child) -> Result<(), String> {
    // Try graceful shutdown first with SIGTERM (allows recorder to finalize WAV header)
    #[cfg(unix)]
    {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;
        if let Some(pid) = child.id() {
            let pid_i32 = pid as i32;
            debug!(
                pid = pid_i32,
                "sending SIGTERM to recorder for graceful shutdown"
            );
            if let Err(e) = kill(Pid::from_raw(pid_i32), Signal::SIGTERM) {
                warn!(pid = pid_i32, error = %e, "failed to send SIGTERM, falling back to SIGKILL");
                child.start_kill().map_err(|e| e.to_string())?;
                child.wait().await.map_err(|e| e.to_string())?;
                return Ok(());
            }
            match tokio::time::timeout(RECORDER_STOP_TIMEOUT, child.wait()).await {
                Ok(Ok(_status)) => {
                    debug!(pid = pid_i32, "recorder exited gracefully after SIGTERM");
                    return Ok(());
                }
                Ok(Err(e)) => {
                    return Err(format!("failed to wait for recorder after SIGTERM: {e}"));
                }
                Err(_) => {
                    warn!(
                        pid = pid_i32,
                        timeout_secs = RECORDER_STOP_TIMEOUT.as_secs(),
                        "recorder did not exit before timeout, sending SIGKILL"
                    );
                }
            }
        }
    }
    // Fallback to SIGKILL (or Windows/non-Unix)
    child
        .start_kill()
        .map_err(|e| format!("failed to kill recorder: {e}"))?;
    child.wait().await.map_err(|e| e.to_string())?;
    Ok(())
}

async fn wav_data_present(path: &Path) -> bool {
    use tokio::io::AsyncReadExt;
    let Ok(file) = fs::File::open(path).await else {
        return false;
    };
    let mut bytes = Vec::new();
    if file.take(65536).read_to_end(&mut bytes).await.is_err() || bytes.get(..4) != Some(b"RIFF") {
        return false;
    }
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        if &bytes[offset..offset + 4] == b"data" {
            return bytes.len() > offset + 9;
        }
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap_or_default())
            as usize;
        offset = offset
            .saturating_add(8)
            .saturating_add(size)
            .saturating_add(size % 2);
    }
    false
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
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        hound::WavReader::open(path)
            .map(|mut reader| {
                reader.spec().sample_rate > 0
                    && reader.samples::<i16>().next().is_some_and(|s| s.is_ok())
            })
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
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

    #[tokio::test]
    async fn spec_005_rejects_header_only_and_invalid_wav() {
        let path = std::env::temp_dir().join(format!("{}.wav", Uuid::new_v4()));
        fs::write(&path, vec![0; 100]).await.unwrap();
        assert!(!wav_data_present(&path).await);
        assert!(!file_has_audio(&path).await);
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        writer.flush().unwrap();
        assert!(!wav_data_present(&path).await);
        writer.write_sample(123_i16).unwrap();
        writer.finalize().unwrap();
        assert!(wav_data_present(&path).await);
        assert!(file_has_audio(&path).await);
        fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn recorder_failure_empty_exit_and_stall_never_report_success() {
        let bus = rustle_core::EventBus::new();
        let mut events = bus.subscribe();
        let mut config = CaptureConfig {
            meeting_id: Uuid::new_v4(),
            meeting_name: "Recorder failure".into(),
            input_device: "default".into(),
            output_device: "default".into(),
            capture_loopback: false,
            recording_started: false,
            chunk_duration: Duration::from_secs(60),
            max_chunk_bytes: MAX_CHUNK_BYTES,
            event_tx: bus.sender(),
        };
        let (_stop, mut stop_rx) = mpsc::channel(1);
        let path = std::env::temp_dir().join(format!("{}.wav", Uuid::new_v4()));
        for script in ["exit 7", "exit 0", "exec sleep 30"] {
            let result = record_chunk(
                &mut config,
                RecorderCommand {
                    program: "sh",
                    args: vec!["-c".into(), script.into()],
                },
                &path,
                &mut stop_rx,
                &mut None,
            )
            .await;
            assert!(result.is_err(), "{script} must fail: {result:?}");
            assert!(!config.recording_started);
            assert!(events.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn spec_005_stop_child_returns_when_process_exits() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 0.1"])
            .kill_on_drop(true)
            .spawn()
            .expect("test child should spawn");

        stop_child(&mut child)
            .await
            .expect("child should stop cleanly");
    }
}
