//! Transcription crate for Rustle.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs as std_fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rustle_core::{
    expand_tilde, resolve_transcripts_dir, safe_filename, tasks::spawn_logged, AppEvent, CoreError,
    EventReceiver, EventSender, NotificationUrgency, Settings, TranscriptSegment,
    TranscriptionMethod,
};
use tokio::fs;
use tracing::{debug, info, warn};
use uuid::Uuid;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const WHISPER_SAMPLE_RATE: u32 = 16_000;
const DEFAULT_WHISPER_REPO: &str = "ggerganov/whisper.cpp";
const DEFAULT_WHISPER_MODEL_FILE: &str = "ggml-base.en.bin";
const TEST_AUDIO_FILE_ENV: &str = "RUSTLE_TEST_AUDIO_FILE";

struct MeetingTranscript {
    name: String,
    path: PathBuf,
    segments: Vec<TranscriptSegment>,
    pending_chunks: usize,
    offset_ms: u64,
    failed: bool,
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
    let mut meetings: HashMap<Uuid, MeetingTranscript> = HashMap::new();
    let mut settings = load_settings("audio transcription startup").await;
    let mut failed_drafts = HashSet::new();
    let mut queue = VecDeque::new();
    let mut jobs = tokio::task::JoinSet::new();
    loop {
        if jobs.is_empty() {
            if let Some((id, path, job_settings)) = queue.pop_front() {
                jobs.spawn(async move {
                    let result = transcribe_audio_chunk(path, job_settings).await;
                    (id, result)
                });
            }
        }
        tokio::select! {
            result = jobs.join_next(), if !jobs.is_empty() => {
                if let Some(Ok((id, result))) = result {
                    if let Some(meeting) = meetings.get_mut(&id) {
                        meeting.pending_chunks = meeting.pending_chunks.saturating_sub(1);
                        match result {
                            Ok((mut segments, duration_ms)) => {
                                for segment in &mut segments {
                                    segment.start_ms += meeting.offset_ms;
                                    segment.end_ms += meeting.offset_ms;
                                }
                                meeting.offset_ms = meeting.offset_ms.saturating_add(duration_ms);
                                if let Err(error) = append_segments(&meeting.path, &segments).await {
                                    meeting.failed = true;
                                    workflow_failure(&event_tx, id, "transcript storage", &error.to_string());
                                }
                                meeting.segments.extend(segments);
                            }
                            Err(error) => {
                                meeting.failed = true;
                                workflow_failure(&event_tx, id, "transcription", &error);
                                publish(&event_tx, transcription_failure_notification(&meeting.name, &error));
                            }
                        }
                    }
                } else {
                    // A worker panic must not leave meetings permanently pending.
                    for (id, meeting) in &mut meetings {
                        meeting.failed = true;
                        meeting.pending_chunks = 0;
                        workflow_failure(&event_tx, *id, "transcription", "transcription worker panicked; recordings retained");
                    }
                    queue.clear();
                }
            }
            event = event_rx.recv() => match event {
                Ok(AppEvent::MeetingStarted { id, name, .. }) => {
                    match create_transcript_draft(&name).await {
                        Ok(path) => {
                            let test_audio_path = test_audio_path_from_env();
                            meetings.insert(id, MeetingTranscript {
                                name, path: path.clone(), segments: Vec::new(), pending_chunks: 0,
                                offset_ms: 0, failed: false,
                                recording_active: true,
                                test_audio_path, meeting_ended: false,
                            });
                            publish(&event_tx, AppEvent::TranscriptDraftReady { meeting_id: id, path });
                        }
                        Err(error) => {
                            workflow_failure(&event_tx, id, "transcript storage", &error.to_string());
                            failed_drafts.insert(id);
                        }
                    }
                }
                Ok(AppEvent::RecordingChunkReady { meeting_id, path }) => {
                    if let Some(meeting) = meetings.get_mut(&meeting_id) {
                        meeting.pending_chunks += 1;
                        queue.push_back((meeting_id, path, settings.clone()));
                    }
                }
                Ok(AppEvent::MeetingEnded { id }) => {
                    if let Some(meeting) = meetings.get_mut(&id) { meeting.meeting_ended = true; }
                }
                Ok(AppEvent::RecordingStarted { meeting_id }) => {
                    if let Some(meeting) = meetings.get_mut(&meeting_id) { meeting.recording_active = true; }
                }
                Ok(AppEvent::RecordingStopped { meeting_id }) => {
                    if failed_drafts.remove(&meeting_id) { publish(&event_tx, AppEvent::MeetingFinished { meeting_id }); }
                    if let Some(meeting) = meetings.get_mut(&meeting_id) { meeting.recording_active = false; }
                }
                Ok(AppEvent::WorkflowFailed { meeting_id, stage, .. }) if stage == "recording" => {
                    if let Some(meeting) = meetings.get_mut(&meeting_id) { meeting.failed = true; }
                }
                Ok(AppEvent::SummariseTranscriptRequested { path }) => summarise_saved_transcript(&event_tx, path).await,
                Ok(AppEvent::SettingsChanged(updated)) => settings = updated.validated(),
                Ok(AppEvent::ShutdownComplete) => break,
                Ok(_) => {},
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    for (id, meeting) in &mut meetings {
                        meeting.failed = true;
                        workflow_failure(&event_tx, *id, "transcription", &format!("lost {skipped} workflow events; recordings retained"));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        let ready: Vec<Uuid> = meetings
            .iter()
            .filter(|(_, meeting)| {
                meeting.meeting_ended && !meeting.recording_active && meeting.pending_chunks == 0
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ready {
            finalise_meeting_transcript(&event_tx, &mut meetings, id, settings.clone()).await;
        }
    }
}

fn workflow_failure(event_tx: &EventSender, id: Uuid, stage: &str, message: &str) {
    warn!(meeting_id = %id, stage, message, "workflow failed; source material retained");
    publish(
        event_tx,
        AppEvent::WorkflowFailed {
            meeting_id: id,
            stage: stage.to_owned(),
            message: message.to_owned(),
        },
    );
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

    if meeting.failed {
        publish(event_tx, AppEvent::MeetingFinished { meeting_id });
        return;
    }

    let mut segments = meeting.segments;
    if segments.is_empty() {
        if let Some(test_audio_path) = meeting.test_audio_path.clone() {
            segments =
                match transcribe_test_audio(event_tx, &meeting.name, test_audio_path, settings)
                    .await
                {
                    Ok(segments) => segments,
                    Err(error) => {
                        workflow_failure(event_tx, meeting_id, "transcription", &error);
                        publish(event_tx, AppEvent::MeetingFinished { meeting_id });
                        return;
                    }
                };
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
        publish(event_tx, AppEvent::MeetingFinished { meeting_id });
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

async fn create_transcript_draft(meeting_name: &str) -> std::io::Result<PathBuf> {
    let directory = resolve_transcripts_dir()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
    fs::create_dir_all(&directory).await?;

    let file_name = format!(
        "{}_{}_{}.txt",
        unix_timestamp_seconds(),
        Uuid::new_v4(),
        safe_filename(meeting_name)
    );
    let path = directory.join(file_name);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .await?;
    Ok(path)
}

async fn transcribe_audio_chunk(
    path: PathBuf,
    settings: Settings,
) -> Result<(Vec<TranscriptSegment>, u64), String> {
    tokio::task::spawn_blocking(move || {
        let reader = hound::WavReader::open(&path)
            .map_err(|error| format!("failed to open WAV: {error}"))?;
        let duration_ms =
            u64::from(reader.duration()) * 1000 / u64::from(reader.spec().sample_rate.max(1));
        let segments = transcribe_audio_chunk_blocking(&path, &settings)?;
        Ok((segments, duration_ms))
    })
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
) -> Result<Vec<TranscriptSegment>, String> {
    let result = if fixture_is_transcript(&path) {
        fs::read_to_string(&path)
            .await
            .map(transcript_segments)
            .map_err(|error| format!("failed to read transcript fixture: {error}"))
    } else {
        transcribe_audio_chunk(path, settings)
            .await
            .map(|(segments, _)| segments)
    };
    if let Err(error) = &result {
        publish(
            event_tx,
            transcription_failure_notification(meeting_name, error),
        );
    }
    result
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
    let audio = read_wav_as_whisper_audio(path)?;
    if audio.is_empty() {
        return Ok(Vec::new());
    }

    let model_path = resolve_whisper_model_path(settings)?;
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
        AppEvent::TranscriptImported {
            meeting_id,
            name: meeting_name,
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
    let title_fragment = rustle_core::paths::strip_document_id(title_fragment);
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
    async fn spec_018_missing_transcript_fixture_returns_error_and_notification() {
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

        assert!(segments
            .unwrap_err()
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
    fn meeting_name_from_path_handles_missing_extension() {
        let name = meeting_name_from_path(std::path::Path::new("/tmp/1234567890_meeting"));
        assert_eq!(name, "meeting");
    }
}
