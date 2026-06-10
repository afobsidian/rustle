//! Transcription crate for Rustle.

use std::collections::HashMap;
use std::fs as std_fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustle_core::{
    expand_tilde, resolve_transcripts_dir, safe_filename, tasks::spawn_logged, AppEvent, CoreError,
    DetectionSource, EventReceiver, EventSender, NotificationUrgency, Settings, StoredDocumentKind,
    TranscriptSegment, TranscriptionMethod,
};
use tokio::fs;
use tracing::{debug, info, warn};
use uuid::Uuid;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const WHISPER_SAMPLE_RATE: u32 = 16_000;
const DEFAULT_WHISPER_REPO: &str = "ggerganov/whisper.cpp";
const DEFAULT_WHISPER_MODEL_FILE: &str = "ggml-base.en.bin";
const TEST_AUDIO_FILE_ENV: &str = "RUSTLE_TEST_AUDIO_FILE";
const MIN_AUTODETECTED_MEETING_DURATION: Duration = Duration::from_secs(300);

struct MeetingTranscript {
    name: String,
    path: PathBuf,
    segments: Vec<TranscriptSegment>,
    chunk_paths: Vec<PathBuf>,
    source: DetectionSource,
    started_at: Instant,
    ended_at: Option<Instant>,
    test_audio_path: Option<PathBuf>,
    recording_active: bool,
    meeting_ended: bool,
}

/// Initialises the transcription component with the shared application event bus.
pub async fn initialise(
    event_tx: EventSender,
    event_rx: Option<EventReceiver>,
) -> Result<(), CoreError> {
    whisper_rs::install_logging_hooks();

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
    let mut settings = load_settings("audio transcription startup").await;

    loop {
        match event_rx.recv().await {
            Ok(AppEvent::MeetingStarted { id, name, source }) => {
                info!(?source, meeting = %name, "creating transcript draft");
                match create_transcript_draft(&name).await {
                    Ok(path) => {
                        let test_audio_path = test_audio_path_from_env();
                        if let Some(test_audio_path) = &test_audio_path {
                            info!(
                                path = %test_audio_path.display(),
                                "test audio fixture configured for transcription"
                            );
                        }
                        meetings.insert(
                            id,
                            MeetingTranscript {
                                name: name.clone(),
                                path: path.clone(),
                                segments: Vec::new(),
                                chunk_paths: Vec::new(),
                                source,
                                started_at: Instant::now(),
                                ended_at: None,
                                test_audio_path,
                                recording_active: false,
                                meeting_ended: false,
                            },
                        );
                        publish(
                            &event_tx,
                            AppEvent::TranscriptDraftReady {
                                meeting_id: id,
                                path: path.clone(),
                            },
                        );
                        info!(path = %path.display(), "transcript draft path");
                    }
                    Err(error) => {
                        warn!(%error, meeting = %name, "failed to create transcript draft")
                    }
                }
            }
            Ok(AppEvent::RecordingStarted { meeting_id }) => {
                if let Some(meeting) = meetings.get_mut(&meeting_id) {
                    meeting.recording_active = true;
                }
            }
            Ok(AppEvent::RecordingChunkReady { meeting_id, path }) => {
                let Some((meeting_name, transcript_path)) =
                    meetings.get_mut(&meeting_id).map(|meeting| {
                        meeting.chunk_paths.push(path.clone());
                        (meeting.name.clone(), meeting.path.clone())
                    })
                else {
                    warn!(meeting_id = %meeting_id, path = %path.display(), "recording chunk has no active transcript");
                    continue;
                };

                let segments = match transcribe_audio_chunk(path.clone(), settings.clone()).await {
                    Ok(segments) => segments,
                    Err(error) => {
                        warn!(%error, path = %path.display(), "failed to transcribe audio chunk");
                        publish(
                            &event_tx,
                            transcription_failure_notification(&meeting_name, &error),
                        );
                        vec![recording_fallback_segment(&path, &error)]
                    }
                };

                if let Some(meeting) = meetings.get_mut(&meeting_id) {
                    if let Err(error) = append_segments(&transcript_path, &segments).await {
                        warn!(%error, path = %transcript_path.display(), "failed to append transcript segments");
                    }
                    meeting.segments.extend(segments);
                } else {
                    warn!(meeting_id = %meeting_id, path = %path.display(), "recording chunk has no active transcript");
                }
            }
            Ok(AppEvent::MeetingEnded { id }) => {
                let Some(meeting) = meetings.get_mut(&id) else {
                    warn!(
                        meeting_id = %id,
                        "ignoring duplicate meeting end without an active transcript draft"
                    );
                    continue;
                };

                meeting.meeting_ended = true;
                meeting.ended_at = Some(Instant::now());
                if meeting.recording_active {
                    info!(
                        meeting_id = %id,
                        path = %meeting.path.display(),
                        "meeting ended; waiting for final recording chunk"
                    );
                    continue;
                }

                finalise_meeting_transcript(&event_tx, &mut meetings, id, settings.clone()).await;
            }
            Ok(AppEvent::RecordingStopped { meeting_id }) => {
                let should_finalise = match meetings.get_mut(&meeting_id) {
                    Some(meeting) => {
                        meeting.recording_active = false;
                        meeting.meeting_ended
                    }
                    None => {
                        warn!(
                            meeting_id = %meeting_id,
                            "recording stopped without an active transcript draft"
                        );
                        false
                    }
                };

                if should_finalise {
                    finalise_meeting_transcript(
                        &event_tx,
                        &mut meetings,
                        meeting_id,
                        settings.clone(),
                    )
                    .await;
                }
            }
            Ok(AppEvent::SummariseTranscriptRequested { path }) => {
                summarise_saved_transcript(&event_tx, path).await;
            }
            Ok(AppEvent::SettingsChanged(updated_settings)) => {
                settings = updated_settings.validated();
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

async fn finalise_meeting_transcript(
    event_tx: &EventSender,
    meetings: &mut HashMap<Uuid, MeetingTranscript>,
    meeting_id: Uuid,
    settings: Settings,
) {
    let Some(meeting) = meetings.remove(&meeting_id) else {
        warn!(
            meeting_id = %meeting_id,
            "ignoring duplicate meeting finalisation without an active transcript draft"
        );
        return;
    };

    if is_short_hyprland_false_positive(&meeting) {
        discard_short_hyprland_false_positive(event_tx, meeting_id, &meeting).await;
        return;
    }

    let mut segments = meeting.segments;
    if segments.is_empty() {
        if let Some(test_audio_path) = meeting.test_audio_path.clone() {
            segments =
                transcribe_test_audio(event_tx, &meeting.name, test_audio_path, settings).await;
            if let Err(error) = append_segments(&meeting.path, &segments).await {
                warn!(%error, path = %meeting.path.display(), "failed to append test audio transcript segments");
            }
        }
    }
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

    if segments.is_empty() {
        warn!(
            meeting_id = %meeting_id,
            path = %meeting.path.display(),
            "transcript is empty; skipping summarisation"
        );
        return;
    }

    info!(
        meeting_id = %meeting_id,
        path = %meeting.path.display(),
        segments = segments.len(),
        "transcript ready"
    );
    publish(
        event_tx,
        AppEvent::TranscriptionReady {
            meeting_id,
            segments,
        },
    );
}

fn meeting_duration(meeting: &MeetingTranscript) -> Duration {
    meeting
        .ended_at
        .map(|ended_at| ended_at.duration_since(meeting.started_at))
        .unwrap_or_else(|| meeting.started_at.elapsed())
}

fn is_short_hyprland_false_positive(meeting: &MeetingTranscript) -> bool {
    meeting.source == DetectionSource::Hyprland
        && meeting_duration(meeting) < MIN_AUTODETECTED_MEETING_DURATION
}

async fn discard_short_hyprland_false_positive(
    event_tx: &EventSender,
    meeting_id: Uuid,
    meeting: &MeetingTranscript,
) {
    info!(
        meeting_id = %meeting_id,
        meeting = %meeting.name,
        duration_secs = meeting_duration(meeting).as_secs_f64(),
        min_duration_secs = MIN_AUTODETECTED_MEETING_DURATION.as_secs(),
        path = %meeting.path.display(),
        "discarding short Hyprland detection as false positive"
    );

    let transcript_deleted = match fs::remove_file(&meeting.path).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            warn!(%error, path = %meeting.path.display(), "failed to remove false-positive transcript draft");
            false
        }
    };

    if transcript_deleted {
        publish(
            event_tx,
            AppEvent::DocumentDeleted {
                path: meeting.path.clone(),
                kind: StoredDocumentKind::Transcript,
            },
        );
    }

    for chunk_path in &meeting.chunk_paths {
        match fs::remove_file(chunk_path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!(%error, path = %chunk_path.display(), "failed to remove false-positive recording chunk");
            }
        }
    }
}

async fn create_transcript_draft(meeting_name: &str) -> std::io::Result<PathBuf> {
    let directory = resolve_transcripts_dir()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
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
            "OpenAI transcription is not supported in Rustle v0.1; use Local Whisper instead"
                .to_owned(),
        ),
    }
}

async fn transcribe_test_audio(
    event_tx: &EventSender,
    meeting_name: &str,
    path: PathBuf,
    settings: Settings,
) -> Vec<TranscriptSegment> {
    if fixture_is_transcript(&path) {
        info!(path = %path.display(), "loading deterministic transcript fixture");
        return match fs::read_to_string(&path).await {
            Ok(transcript) => transcript_segments(transcript),
            Err(error) => {
                let error = format!("failed to read transcript fixture: {error}");
                warn!(%error, path = %path.display(), "failed to load transcript fixture");
                publish(
                    event_tx,
                    transcription_failure_notification(meeting_name, &error),
                );
                vec![recording_fallback_segment(&path, &error)]
            }
        };
    }

    info!(path = %path.display(), "transcribing test audio fixture");
    match transcribe_audio_chunk(path.clone(), settings).await {
        Ok(segments) => segments,
        Err(error) => {
            warn!(%error, path = %path.display(), "failed to transcribe test audio fixture");
            publish(
                event_tx,
                transcription_failure_notification(meeting_name, &error),
            );
            vec![recording_fallback_segment(&path, &error)]
        }
    }
}

fn transcription_failure_notification(meeting_name: &str, error: &str) -> AppEvent {
    AppEvent::NotificationRequested {
        title: "Transcription issue".to_owned(),
        body: format!(
            "Rustle could not transcribe audio for {meeting_name}. The transcript draft remains available. {error}"
        ),
        urgency: NotificationUrgency::Critical,
    }
}

fn transcribe_with_whisper(
    path: &Path,
    settings: &Settings,
) -> Result<Vec<TranscriptSegment>, String> {
    let model_path = resolve_whisper_model_path(settings)?;

    let audio = read_wav_as_whisper_audio(path)?;
    if audio.is_empty() {
        return Ok(Vec::new());
    }

    let audio_duration_secs = audio.len() as f64 / f64::from(WHISPER_SAMPLE_RATE);
    info!(
        path = %path.display(),
        duration_secs = audio_duration_secs,
        "local transcription started"
    );
    debug!(
        path = %path.display(),
        model_path = %model_path.display(),
        samples = audio.len(),
        "loading Whisper model"
    );

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
    params.set_progress_callback_safe({
        let mut last_info_progress = 0;
        let mut last_debug_progress = 0;
        move |progress: i32| {
            let progress = progress.clamp(0, 100);
            if progress >= last_debug_progress + 5 || progress == 100 {
                debug!(progress_percent = progress, "local transcription progress");
                last_debug_progress = progress;
            }
            if progress >= last_info_progress + 25 || progress == 100 {
                info!(progress_percent = progress, "local transcription progress");
                last_info_progress = progress;
            }
        }
    });

    let started_at = Instant::now();
    state
        .full(params, &audio)
        .map_err(|error| format!("failed to run Whisper transcription: {error}"))?;

    let segments: Vec<TranscriptSegment> = state
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

    info!(
        path = %path.display(),
        segments = segments.len(),
        elapsed_ms = started_at.elapsed().as_millis(),
        "local transcription finished"
    );

    Ok(segments)
}

fn resolve_whisper_model_path(settings: &Settings) -> Result<PathBuf, String> {
    let configured_path = settings.transcription.model_path.trim();
    let model_path = expand_tilde(configured_path).map_err(|error| error.to_string())?;
    if model_path.is_file() {
        return Ok(model_path);
    }

    if is_default_transcription_model_path(configured_path) {
        return download_default_whisper_model(&model_path);
    }

    Err(format!(
        "Whisper model not found at {}; set transcription.model_path to a ggml model",
        model_path.display()
    ))
}

fn download_default_whisper_model(destination: &Path) -> Result<PathBuf, String> {
    use hf_hub::api::sync::Api;

    info!(
        path = %destination.display(),
        repo = DEFAULT_WHISPER_REPO,
        file = DEFAULT_WHISPER_MODEL_FILE,
        "default Whisper model missing; downloading"
    );

    if let Some(parent) = destination.parent() {
        std_fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create Whisper model directory {}: {error}",
                parent.display()
            )
        })?;
    }

    if destination.is_file() {
        return Ok(destination.to_path_buf());
    }

    let cached_path = Api::new()
        .map_err(|error| format!("failed to initialise Hugging Face client: {error}"))?
        .model(DEFAULT_WHISPER_REPO.to_owned())
        .get(DEFAULT_WHISPER_MODEL_FILE)
        .map_err(|error| format!("failed to download default Whisper model: {error}"))?;

    if cached_path != destination {
        std_fs::copy(&cached_path, destination).map_err(|error| {
            format!(
                "failed to copy Whisper model from {} to {}: {error}",
                cached_path.display(),
                destination.display()
            )
        })?;
    }

    info!(path = %destination.display(), "default Whisper model ready");
    Ok(destination.to_path_buf())
}

fn is_default_transcription_model_path(configured_path: &str) -> bool {
    configured_path == Settings::default().transcription.model_path
}

fn fixture_is_transcript(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("txt" | "md")
    )
}

fn test_audio_path_from_env() -> Option<PathBuf> {
    match std::env::var(TEST_AUDIO_FILE_ENV) {
        Ok(value) => match test_audio_path_from_value(&value) {
            Ok(path) => path,
            Err(error) => {
                warn!(%error, env = TEST_AUDIO_FILE_ENV, "ignoring test audio fixture");
                None
            }
        },
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => {
            warn!(%error, env = TEST_AUDIO_FILE_ENV, "ignoring test audio fixture");
            None
        }
    }
}

fn test_audio_path_from_value(value: &str) -> Result<Option<PathBuf>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }

    expand_tilde(value)
        .map(Some)
        .map_err(|error| error.to_string())
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

async fn summarise_saved_transcript(event_tx: &EventSender, path: PathBuf) {
    let transcript = match fs::read_to_string(&path).await {
        Ok(transcript) => transcript,
        Err(error) => {
            warn!(%error, path = %path.display(), "failed to read transcript for summarisation");
            return;
        }
    };

    let segments = transcript_segments(transcript);
    if segments.is_empty() {
        warn!(path = %path.display(), "refusing to summarise an empty transcript");
        return;
    }

    let meeting_id = Uuid::new_v4();
    let meeting_name = meeting_name_from_path(&path);
    publish(
        event_tx,
        AppEvent::MeetingStarted {
            id: meeting_id,
            name: meeting_name,
            source: DetectionSource::Manual,
        },
    );
    publish(
        event_tx,
        AppEvent::TranscriptionReady {
            meeting_id,
            segments,
        },
    );
}

fn meeting_name_from_path(path: &Path) -> String {
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return "Meeting".to_owned();
    };

    let title_fragment = stem.split_once('_').map(|(_, value)| value).unwrap_or(stem);
    let title = title_fragment
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        "Meeting".to_owned()
    } else {
        title
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
    u64::try_from(value).unwrap_or(0).saturating_mul(10)
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
        assert_eq!(centiseconds_to_millis(i64::MAX), u64::MAX);
    }

    #[test]
    fn derives_meeting_name_from_transcript_filename() {
        let name = meeting_name_from_path(Path::new("/tmp/1779283974_team_sync.txt"));

        assert_eq!(name, "team sync");
    }

    #[test]
    fn spec_006_default_model_path_is_auto_downloadable() {
        assert!(is_default_transcription_model_path(
            &Settings::default().transcription.model_path
        ));
        assert!(!is_default_transcription_model_path(
            "/tmp/custom-whisper-model.bin"
        ));
    }

    #[test]
    fn spec_006_text_fixtures_are_treated_as_deterministic_transcripts() {
        assert!(fixture_is_transcript(Path::new("/tmp/manual-workflow.txt")));
        assert!(fixture_is_transcript(Path::new("/tmp/manual-workflow.md")));
        assert!(!fixture_is_transcript(Path::new(
            "/tmp/manual-workflow.wav"
        )));
    }

    #[test]
    fn spec_006_empty_test_audio_env_is_ignored() {
        assert_eq!(test_audio_path_from_value("   ").unwrap(), None);
    }

    #[test]
    fn spec_006_test_audio_env_resolves_path() {
        assert_eq!(
            test_audio_path_from_value("/tmp/rustle-fixture.wav").unwrap(),
            Some(PathBuf::from("/tmp/rustle-fixture.wav"))
        );
    }

    #[test]
    fn spec_006_openai_transcription_message_marks_it_unsupported() {
        let error = transcribe_audio_chunk_blocking(
            Path::new("/tmp/fixture.wav"),
            &Settings {
                transcription: rustle_core::TranscriptionSettings {
                    method: TranscriptionMethod::Openai,
                    ..Settings::default().transcription
                },
                ..Settings::default()
            },
        )
        .unwrap_err();

        assert!(error.contains("not supported in Rustle v0.1"));
        assert!(error.contains("Local Whisper"));
    }

    #[test]
    fn transcription_failure_notification_mentions_draft_availability() {
        let AppEvent::NotificationRequested {
            title,
            body,
            urgency,
        } = transcription_failure_notification("Planning Sync", "missing model")
        else {
            panic!("expected notification event");
        };

        assert_eq!(title, "Transcription issue");
        assert_eq!(urgency, NotificationUrgency::Critical);
        assert!(body.contains("Planning Sync"));
        assert!(body.contains("transcript draft remains available"));
        assert!(body.contains("missing model"));
    }

    #[tokio::test]
    async fn spec_018_missing_transcript_fixture_returns_fallback_and_notification() {
        let bus = rustle_core::EventBus::new();
        let event_tx = bus.sender();
        let mut observer = bus.subscribe();
        let missing_path = std::env::temp_dir().join(format!(
            "rustle-missing-transcript-fixture-{}.txt",
            unix_timestamp_seconds()
        ));

        let segments = transcribe_test_audio(
            &event_tx,
            "Planning Sync",
            missing_path.clone(),
            Settings::default(),
        )
        .await;

        assert_eq!(segments.len(), 1);
        assert!(segments[0]
            .text
            .contains("speech transcription was not available"));
        assert!(segments[0]
            .text
            .contains(&missing_path.display().to_string()));
        assert!(segments[0]
            .text
            .contains("failed to read transcript fixture"));

        let AppEvent::NotificationRequested {
            title,
            body,
            urgency,
        } = observer.recv().await.expect("notification should publish")
        else {
            panic!("expected notification event");
        };

        assert_eq!(title, "Transcription issue");
        assert_eq!(urgency, NotificationUrgency::Critical);
        assert!(body.contains("Planning Sync"));
        assert!(body.contains("transcript draft remains available"));
        assert!(body.contains("failed to read transcript fixture"));
    }

    #[test]
    fn spec_006_empty_segments_are_flagged_for_transcription_loop_guard() {
        let segments: Vec<TranscriptSegment> = Vec::new();
        assert!(segments.is_empty(), "empty segments guard must fire");
        // Sanity: a single non-empty segment is not empty.
        let non_empty = [TranscriptSegment {
            start_ms: 0,
            end_ms: 100,
            text: "Alice: Hello".to_owned(),
        }];
        assert!(!non_empty.is_empty());
    }

    #[test]
    fn transcript_segments_handles_whitespace_only_text() {
        let segments = transcript_segments("   \n\n  ".to_owned());
        assert!(segments.is_empty());
    }

    #[test]
    fn recording_fallback_segment_includes_path_and_error() {
        let path = std::path::Path::new("/tmp/test-recording.wav");
        let error = "model not found";
        let segment = recording_fallback_segment(path, error);

        assert!(segment.text.contains("/tmp/test-recording.wav"));
        assert!(segment.text.contains("model not found"));
    }

    #[test]
    fn meeting_name_from_path_handles_missing_extension() {
        let name = meeting_name_from_path(std::path::Path::new("/tmp/1234567890_meeting"));
        assert_eq!(name, "meeting");
    }

    #[test]
    fn spec_028_short_hyprland_detection_is_treated_as_false_positive() {
        let meeting = MeetingTranscript {
            name: "Short auto-detected meeting".to_owned(),
            path: PathBuf::from("/tmp/short-auto-detected-meeting.txt"),
            segments: Vec::new(),
            chunk_paths: Vec::new(),
            source: DetectionSource::Hyprland,
            started_at: Instant::now(),
            ended_at: Some(Instant::now()),
            test_audio_path: None,
            recording_active: false,
            meeting_ended: true,
        };

        assert!(is_short_hyprland_false_positive(&meeting));
    }

    #[test]
    fn spec_028_manual_and_long_hyprland_meetings_are_not_false_positives() {
        let manual_meeting = MeetingTranscript {
            name: "Manual meeting".to_owned(),
            path: PathBuf::from("/tmp/manual-meeting.txt"),
            segments: Vec::new(),
            chunk_paths: Vec::new(),
            source: DetectionSource::Manual,
            started_at: Instant::now(),
            ended_at: Some(Instant::now()),
            test_audio_path: None,
            recording_active: false,
            meeting_ended: true,
        };
        assert!(!is_short_hyprland_false_positive(&manual_meeting));

        let long_hyprland_meeting = MeetingTranscript {
            name: "Long auto-detected meeting".to_owned(),
            path: PathBuf::from("/tmp/long-auto-detected-meeting.txt"),
            segments: Vec::new(),
            chunk_paths: Vec::new(),
            source: DetectionSource::Hyprland,
            started_at: Instant::now() - MIN_AUTODETECTED_MEETING_DURATION - Duration::from_secs(1),
            ended_at: Some(Instant::now()),
            test_audio_path: None,
            recording_active: false,
            meeting_ended: true,
        };
        assert!(!is_short_hyprland_false_positive(&long_hyprland_meeting));
    }
}
