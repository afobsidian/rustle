//! Transcription crate for Rustle.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rustle_core::{
    default_data_dir, expand_tilde, safe_filename, tasks::spawn_logged, AppEvent, CoreError,
    EventReceiver, EventSender, Settings, TranscriptSegment, TranscriptionMethod,
};
use tokio::fs;
use tokio::process::Command;
use tracing::{info, warn};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const WHISPER_SAMPLE_RATE: u32 = 16_000;

struct MeetingTranscript {
    path: PathBuf,
    segments: Vec<TranscriptSegment>,
}

/// Initialises the transcription component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    if let Some(receiver) = event_rx {
        spawn_logged(
            "audio-transcription-loop",
            transcription_loop(event_tx, receiver),
        );
    }

    Ok(())
}

async fn transcription_loop(event_tx: EventSender, mut event_rx: EventReceiver) {
    let mut meetings = HashMap::new();

    loop {
        match event_rx.recv().await {
            Ok(AppEvent::MeetingStarted { id, name, source }) => {
                info!(?source, meeting = %name, "creating transcript draft");
                match create_transcript_draft(&name).await {
                    Ok(path) => {
                        meetings.insert(
                            id,
                            MeetingTranscript {
                                path: path.clone(),
                                segments: Vec::new(),
                            },
                        );
                        publish(
                            &event_tx,
                            AppEvent::TranscriptDraftReady {
                                meeting_id: id,
                                path: path.clone(),
                            },
                        );
                        open_path(&path).await;
                    }
                    Err(error) => {
                        warn!(%error, meeting = %name, "failed to create transcript draft")
                    }
                }
            }
            Ok(AppEvent::RecordingChunkReady { meeting_id, path }) => {
                let settings = load_settings("audio transcription").await;
                let segments = match transcribe_audio_chunk(path.clone(), settings).await {
                    Ok(segments) => segments,
                    Err(error) => {
                        warn!(%error, path = %path.display(), "failed to transcribe audio chunk");
                        vec![recording_fallback_segment(&path, &error)]
                    }
                };

                if let Some(meeting) = meetings.get_mut(&meeting_id) {
                    if let Err(error) = append_segments(&meeting.path, &segments).await {
                        warn!(%error, path = %meeting.path.display(), "failed to append transcript segments");
                    }
                    meeting.segments.extend(segments);
                } else {
                    warn!(meeting_id = %meeting_id, path = %path.display(), "recording chunk has no active transcript");
                }
            }
            Ok(AppEvent::MeetingEnded { id }) => {
                let Some(meeting) = meetings.remove(&id) else {
                    warn!(meeting_id = %id, "meeting ended without a transcript draft");
                    publish(
                        &event_tx,
                        AppEvent::TranscriptionReady {
                            meeting_id: id,
                            segments: Vec::new(),
                        },
                    );
                    continue;
                };

                let mut segments = meeting.segments;
                match fs::read_to_string(&meeting.path).await {
                    Ok(content) => {
                        let manual_segments = transcript_segments(content);
                        if segments.is_empty() {
                            segments = manual_segments;
                        }
                    }
                    Err(error) => {
                        warn!(%error, path = %meeting.path.display(), "failed to read transcript draft");
                    }
                }

                info!(
                    meeting_id = %id,
                    path = %meeting.path.display(),
                    segments = segments.len(),
                    "transcript ready"
                );
                publish(
                    &event_tx,
                    AppEvent::TranscriptionReady {
                        meeting_id: id,
                        segments,
                    },
                );
            }
            Ok(AppEvent::QuitRequested) => break,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(skipped, "transcription loop lagged behind event bus");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn create_transcript_draft(meeting_name: &str) -> std::io::Result<PathBuf> {
    let directory = default_data_dir()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?
        .join("transcripts");
    fs::create_dir_all(&directory).await?;

    let file_name = format!(
        "{}_{}.txt",
        unix_timestamp_seconds(),
        safe_filename(meeting_name)
    );
    let path = directory.join(file_name);
    fs::write(&path, "").await?;
    Ok(path)
}

async fn transcribe_audio_chunk(
    path: PathBuf,
    settings: Settings,
) -> Result<Vec<TranscriptSegment>, String> {
    tokio::task::spawn_blocking(move || transcribe_audio_chunk_blocking(&path, &settings))
        .await
        .map_err(|error| format!("transcription worker failed: {error}"))?
}

fn transcribe_audio_chunk_blocking(
    path: &Path,
    settings: &Settings,
) -> Result<Vec<TranscriptSegment>, String> {
    match settings.transcription.method {
        TranscriptionMethod::Local => transcribe_with_whisper(path, settings),
        TranscriptionMethod::Openai => Err(
            "OpenAI transcription is configured, but Rust-native local transcription is implemented first"
                .to_owned(),
        ),
    }
}

fn transcribe_with_whisper(
    path: &Path,
    settings: &Settings,
) -> Result<Vec<TranscriptSegment>, String> {
    let model_path =
        expand_tilde(&settings.transcription.model_path).map_err(|error| error.to_string())?;
    if !model_path.is_file() {
        return Err(format!(
            "Whisper model not found at {}; set transcription.model_path to a ggml model",
            model_path.display()
        ));
    }

    let audio = read_wav_as_whisper_audio(path)?;
    if audio.is_empty() {
        return Ok(Vec::new());
    }

    let model_path = model_path
        .to_str()
        .ok_or_else(|| "Whisper model path is not valid UTF-8".to_owned())?;
    let ctx = WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
        .map_err(|error| format!("failed to load Whisper model: {error}"))?;
    let mut state = ctx
        .create_state()
        .map_err(|error| format!("failed to create Whisper state: {error}"))?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state
        .full(params, &audio)
        .map_err(|error| format!("failed to run Whisper transcription: {error}"))?;

    let segments = state
        .as_iter()
        .filter_map(|segment| {
            let text = segment.to_str_lossy().ok()?.trim().to_owned();
            if text.is_empty() {
                return None;
            }

            Some(TranscriptSegment {
                start_ms: centiseconds_to_millis(segment.start_timestamp()),
                end_ms: centiseconds_to_millis(segment.end_timestamp()),
                text,
            })
        })
        .collect();

    Ok(segments)
}

fn read_wav_as_whisper_audio(path: &Path) -> Result<Vec<f32>, String> {
    let reader =
        hound::WavReader::open(path).map_err(|error| format!("failed to open WAV: {error}"))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let samples = wav_samples(reader, spec)?;
    let mono = mix_to_mono(&samples, channels);
    Ok(resample_linear(
        &mono,
        spec.sample_rate,
        WHISPER_SAMPLE_RATE,
    ))
}

fn wav_samples<R: std::io::Read + std::io::Seek>(
    reader: hound::WavReader<R>,
    spec: hound::WavSpec,
) -> Result<Vec<f32>, String> {
    match spec.sample_format {
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .map(|sample| sample.map_err(|error| error.to_string()))
            .collect(),
        hound::SampleFormat::Int if spec.bits_per_sample <= 16 => reader
            .into_samples::<i16>()
            .map(|sample| {
                sample
                    .map(|value| f32::from(value) / f32::from(i16::MAX))
                    .map_err(|error| error.to_string())
            })
            .collect(),
        hound::SampleFormat::Int => {
            let max_value = ((1_i64 << (spec.bits_per_sample.saturating_sub(1))) - 1) as f32;
            reader
                .into_samples::<i32>()
                .map(|sample| {
                    sample
                        .map(|value| value as f32 / max_value)
                        .map_err(|error| error.to_string())
                })
                .collect()
        }
    }
}

fn mix_to_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }

    samples
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect()
}

fn resample_linear(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if samples.is_empty() || source_rate == target_rate {
        return samples.to_vec();
    }

    let ratio = f64::from(source_rate) / f64::from(target_rate);
    let output_len = ((samples.len() as f64) / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(output_len);

    for index in 0..output_len {
        let position = index as f64 * ratio;
        let lower = position.floor() as usize;
        let upper = (lower + 1).min(samples.len().saturating_sub(1));
        let fraction = (position - lower as f64) as f32;
        let sample = samples[lower] + (samples[upper] - samples[lower]) * fraction;
        output.push(sample);
    }

    output
}

async fn append_segments(path: &Path, segments: &[TranscriptSegment]) -> std::io::Result<()> {
    let mut content = String::new();
    for segment in segments {
        content.push_str(&format!(
            "[{}ms - {}ms] {}\n",
            segment.start_ms, segment.end_ms, segment.text
        ));
    }

    if content.is_empty() {
        return Ok(());
    }

    use tokio::io::AsyncWriteExt;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .await?;
    file.write_all(content.as_bytes()).await
}

fn recording_fallback_segment(path: &Path, error: &str) -> TranscriptSegment {
    TranscriptSegment {
        start_ms: 0,
        end_ms: 0,
        text: format!(
            "Audio was recorded at {}, but speech transcription was not available: {}",
            path.display(),
            error
        ),
    }
}

fn transcript_segments(transcript: String) -> Vec<TranscriptSegment> {
    let cleaned = transcript.trim().to_owned();
    if cleaned.is_empty() {
        return Vec::new();
    }

    vec![TranscriptSegment {
        start_ms: 0,
        end_ms: 0,
        text: cleaned,
    }]
}

async fn open_path(path: &PathBuf) {
    match Command::new("xdg-open")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_child) => info!(path = %path.display(), "opened transcript draft"),
        Err(error) => warn!(%error, path = %path.display(), "failed to open transcript draft"),
    }
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

fn centiseconds_to_millis(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0) * 10
}

fn unix_timestamp_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_006_mix_to_mono_averages_channels() {
        let mixed = mix_to_mono(&[1.0, -1.0, 0.25, 0.75], 2);

        assert_eq!(mixed, vec![0.0, 0.5]);
    }

    #[test]
    fn spec_006_resample_linear_downsamples_audio() {
        let resampled = resample_linear(&[0.0, 1.0, 0.0, -1.0], 4, 2);

        assert_eq!(resampled, vec![0.0, 0.0]);
    }

    #[test]
    fn spec_006_centiseconds_are_converted_to_milliseconds() {
        assert_eq!(centiseconds_to_millis(42), 420);
        assert_eq!(centiseconds_to_millis(-1), 0);
    }
}
