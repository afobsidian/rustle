//! Audio capture crate for Rustle.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rustle_core::{
    default_data_dir, safe_filename, tasks::spawn_logged, AppEvent, CoreError, EventReceiver,
    EventSender, Settings,
};
use tracing::{info, warn};
use uuid::Uuid;

const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);

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

struct AudioChunk {
    samples: Vec<f32>,
}

struct ChunkWriter {
    directory: PathBuf,
    file_stem: String,
    sample_rate: u32,
    channels: u16,
    max_samples: u64,
    max_bytes: u64,
    event_tx: EventSender,
    meeting_id: Uuid,
    chunk_index: u64,
    samples_in_chunk: u64,
    writer: Option<hound::WavWriter<BufWriter<File>>>,
    current_path: Option<PathBuf>,
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

                let settings = load_settings("audio capture").await;
                if !settings.meeting.auto_capture {
                    info!(meeting = %name, "auto audio capture disabled");
                    continue;
                }

                match start_recording(id, name, settings, event_tx.clone()).await {
                    Ok(recording) => active = Some(recording),
                    Err(error) => warn!(%error, meeting_id = %id, "failed to start audio capture"),
                }
            }
            Ok(AppEvent::MeetingEnded { id }) => {
                if let Some(recording) = active.take() {
                    if recording.meeting_id == id {
                        stop_recording(recording);
                    } else {
                        warn!(meeting_id = %id, active_id = %recording.meeting_id, "meeting ended while another recording was active");
                        active = Some(recording);
                    }
                }
            }
            Ok(AppEvent::QuitRequested) => {
                if let Some(recording) = active.take() {
                    stop_recording(recording);
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

async fn start_recording(
    meeting_id: Uuid,
    meeting_name: String,
    settings: Settings,
    event_tx: EventSender,
) -> Result<ActiveRecording, CoreError> {
    let (stop_tx, stop_rx) = mpsc::channel();
    let config = CaptureConfig {
        meeting_id,
        meeting_name,
        input_device: settings.audio.input_device,
        chunk_duration: Duration::from_secs(settings.audio.chunk_duration_minutes * 60),
        max_chunk_bytes: settings.audio.max_recording_size_mb * 1024 * 1024,
        event_tx,
    };

    tokio::task::spawn_blocking(move || run_capture(config, stop_rx));

    Ok(ActiveRecording {
        meeting_id,
        stop_tx,
    })
}

fn stop_recording(recording: ActiveRecording) {
    if recording.stop_tx.send(()).is_err() {
        warn!(meeting_id = %recording.meeting_id, "audio capture worker already stopped");
    }
}

fn run_capture(config: CaptureConfig, stop_rx: mpsc::Receiver<()>) {
    match capture_until_stopped(config, stop_rx) {
        Ok(()) => {}
        Err(error) => warn!(%error, "audio capture worker stopped with an error"),
    }
}

fn capture_until_stopped(config: CaptureConfig, stop_rx: mpsc::Receiver<()>) -> Result<(), String> {
    let host = cpal::default_host();
    let device = input_device(&host, &config.input_device)?;
    let device_name = device.name().unwrap_or_else(|_| "unknown".to_owned());
    let supported_config = device.default_input_config().map_err(|error| {
        format!("failed to read default input config for {device_name}: {error}")
    })?;
    let sample_rate = supported_config.sample_rate().0;
    let channels = supported_config.channels();
    let (audio_tx, audio_rx) = mpsc::channel();
    let stream_config = supported_config.config();
    let sample_format = supported_config.sample_format();
    let err_fn = |error| warn!(%error, "audio input stream error");

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| send_samples(&audio_tx, data.iter().copied()),
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _| {
                send_samples(
                    &audio_tx,
                    data.iter()
                        .map(|sample| f32::from(*sample) / f32::from(i16::MAX)),
                );
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _| {
                send_samples(
                    &audio_tx,
                    data.iter()
                        .map(|sample| (f32::from(*sample) - 32768.0) / 32768.0),
                );
            },
            err_fn,
            None,
        ),
        other => return Err(format!("unsupported input sample format: {other:?}")),
    }
    .map_err(|error| format!("failed to build input stream for {device_name}: {error}"))?;

    let mut writer = ChunkWriter::new(&config, sample_rate, channels)?;
    stream
        .play()
        .map_err(|error| format!("failed to start input stream for {device_name}: {error}"))?;
    publish(
        &config.event_tx,
        AppEvent::RecordingStarted {
            meeting_id: config.meeting_id,
        },
    );
    info!(
        meeting_id = %config.meeting_id,
        device = %device_name,
        sample_rate,
        channels,
        "audio recording started"
    );

    loop {
        if stop_rx.try_recv().is_ok() {
            break;
        }

        match audio_rx.recv_timeout(STOP_POLL_INTERVAL) {
            Ok(chunk) => writer.write_samples(&chunk.samples)?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    drop(stream);
    writer.finish_current_chunk()?;
    publish(
        &config.event_tx,
        AppEvent::RecordingStopped {
            meeting_id: config.meeting_id,
        },
    );
    info!(meeting_id = %config.meeting_id, "audio recording stopped");
    Ok(())
}

fn input_device(host: &cpal::Host, configured_name: &str) -> Result<cpal::Device, String> {
    let configured_name = configured_name.trim();
    if configured_name.is_empty() || configured_name.eq_ignore_ascii_case("default") {
        return host
            .default_input_device()
            .ok_or_else(|| "no default input device is available".to_owned());
    }

    let mut devices = host
        .input_devices()
        .map_err(|error| format!("failed to enumerate input devices: {error}"))?;

    devices
        .find(|device| {
            device
                .name()
                .map(|name| name == configured_name || name.contains(configured_name))
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("input device not found: {configured_name}"))
}

fn send_samples(samples_tx: &mpsc::Sender<AudioChunk>, samples: impl Iterator<Item = f32>) {
    let samples = samples.collect::<Vec<_>>();
    if samples.is_empty() {
        return;
    }

    if samples_tx.send(AudioChunk { samples }).is_err() {
        warn!("audio writer stopped before input stream");
    }
}

impl ChunkWriter {
    fn new(config: &CaptureConfig, sample_rate: u32, channels: u16) -> Result<Self, String> {
        let directory = default_data_dir()
            .map_err(|error| error.to_string())?
            .join("recordings");
        fs::create_dir_all(&directory)
            .map_err(|error| format!("failed to create recordings directory: {error}"))?;
        let seconds = config.chunk_duration.as_secs().max(1);
        let max_samples = u64::from(sample_rate) * u64::from(channels) * seconds;
        let file_stem = format!(
            "{}_{}",
            unix_timestamp_seconds(),
            safe_filename(&config.meeting_name)
        );

        let mut writer = Self {
            directory,
            file_stem,
            sample_rate,
            channels,
            max_samples,
            max_bytes: config.max_chunk_bytes,
            event_tx: config.event_tx.clone(),
            meeting_id: config.meeting_id,
            chunk_index: 0,
            samples_in_chunk: 0,
            writer: None,
            current_path: None,
        };
        writer.start_next_chunk()?;
        Ok(writer)
    }

    fn write_samples(&mut self, samples: &[f32]) -> Result<(), String> {
        for sample in samples {
            if self.should_rotate() {
                self.finish_current_chunk()?;
                self.start_next_chunk()?;
            }

            let writer = self
                .writer
                .as_mut()
                .ok_or_else(|| "recording chunk writer is not open".to_owned())?;
            writer
                .write_sample(float_to_i16(*sample))
                .map_err(|error| format!("failed to write audio sample: {error}"))?;
            self.samples_in_chunk += 1;
        }

        Ok(())
    }

    fn start_next_chunk(&mut self) -> Result<(), String> {
        self.chunk_index += 1;
        self.samples_in_chunk = 0;
        let path = self.directory.join(format!(
            "{}_chunk_{:04}.wav",
            self.file_stem, self.chunk_index
        ));
        let spec = hound::WavSpec {
            channels: self.channels,
            sample_rate: self.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(&path, spec)
            .map_err(|error| format!("failed to create recording chunk: {error}"))?;
        self.current_path = Some(path);
        self.writer = Some(writer);
        Ok(())
    }

    fn finish_current_chunk(&mut self) -> Result<(), String> {
        let Some(writer) = self.writer.take() else {
            return Ok(());
        };
        let Some(path) = self.current_path.take() else {
            return Ok(());
        };
        let has_samples = self.samples_in_chunk > 0;
        writer
            .finalize()
            .map_err(|error| format!("failed to finalise recording chunk: {error}"))?;

        if has_samples {
            publish(
                &self.event_tx,
                AppEvent::RecordingChunkReady {
                    meeting_id: self.meeting_id,
                    path,
                },
            );
        } else if let Err(error) = fs::remove_file(&path) {
            warn!(%error, path = %path.display(), "failed to remove empty recording chunk");
        }
        self.samples_in_chunk = 0;
        Ok(())
    }

    fn should_rotate(&self) -> bool {
        if self.samples_in_chunk >= self.max_samples {
            return true;
        }

        self.current_path
            .as_deref()
            .and_then(file_len)
            .map(|bytes| bytes >= self.max_bytes)
            .unwrap_or(false)
    }
}

fn file_len(path: &Path) -> Option<u64> {
    fs::metadata(path).map(|metadata| metadata.len()).ok()
}

fn float_to_i16(sample: f32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * f32::from(i16::MAX)).round() as i16
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
    fn spec_005_float_to_i16_clamps_samples() {
        assert_eq!(float_to_i16(2.0), i16::MAX);
        assert_eq!(float_to_i16(-2.0), -i16::MAX);
        assert_eq!(float_to_i16(0.0), 0);
    }
}
