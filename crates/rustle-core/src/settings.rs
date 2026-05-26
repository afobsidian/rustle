//! Persistent Rustle settings and TOML loading helpers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tracing::warn;

use crate::errors::CoreError;

const APP_CONFIG_DIR: &str = "rustle";
const CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_TRANSCRIPTION_MODEL_PATH: &str = "~/.local/share/rustle/models/ggml-base.en.bin";
const DEFAULT_NOTES_DIR: &str = "~/.local/share/rustle/notes";
const DEFAULT_DB_PATH: &str = "~/.local/share/rustle/rustle.db";
const DEFAULT_SYSTEM_PROMPT: &str = "You are a meeting notes assistant. Produce concise, structured notes with a summary, key decisions, action items, and attendees when available.";

/// All persisted user settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    /// General application settings.
    pub general: GeneralSettings,
    /// Meeting detection and capture settings.
    pub meeting: MeetingSettings,
    /// Audio capture settings.
    pub audio: AudioSettings,
    /// Transcription settings.
    pub transcription: TranscriptionSettings,
    /// AI summarisation settings.
    pub ai: AiSettings,
    /// Storage location settings.
    pub storage: StorageSettings,
}

/// General application settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GeneralSettings {
    /// Whether Rustle should start when the user logs in.
    pub start_on_login: bool,
    /// Login startup integration to use.
    pub start_on_login_method: LoginMethod,
}

/// Supported login startup integration mechanisms.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LoginMethod {
    /// XDG autostart desktop file integration.
    #[default]
    Xdg,
    /// systemd user service integration.
    Systemd,
}

/// Meeting detection and capture settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MeetingSettings {
    /// Whether meetings should be detected automatically.
    pub auto_detect: bool,
    /// Whether recording should start automatically when a meeting is detected.
    pub auto_capture: bool,
    /// Preferred meeting detection method.
    pub detection_method: RecordingDetectionMethod,
}

/// Supported meeting detection methods.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RecordingDetectionMethod {
    /// Hyprland IPC and client inspection.
    #[default]
    #[serde(alias = "pipewire", alias = "process")]
    Hyprland,
}

/// Audio capture settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AudioSettings {
    /// Capture input device name.
    pub input_device: String,
    /// Whether to capture from a loopback device.
    pub capture_loopback: bool,
    /// Maximum recording size in megabytes before rotation.
    pub max_recording_size_mb: u64,
    /// Recording chunk duration in minutes.
    pub chunk_duration_minutes: u64,
}

/// Transcription provider settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TranscriptionSettings {
    /// Transcription method to use.
    pub method: TranscriptionMethod,
    /// Local Whisper model path.
    pub model_path: String,
    /// OpenAI API key used for remote transcription.
    pub openai_api_key: String,
}

/// Supported transcription methods.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptionMethod {
    /// Local Whisper transcription.
    #[default]
    Local,
    /// OpenAI Whisper API transcription.
    Openai,
}

/// AI summarisation settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AiSettings {
    /// AI provider to use.
    pub provider: AiProvider,
    /// Provider model name.
    pub model: String,
    /// Optional local model path for providers that load files directly.
    pub model_path: String,
    /// Optional Hugging Face repository for providers that can download models.
    pub hf_repo: String,
    /// Optional Hugging Face model file name to download from `hf_repo`.
    pub hf_model_file: String,
    /// API key for hosted providers.
    pub api_key: String,
    /// Ollama endpoint URL, used only when `provider = "ollama"`.
    pub ollama_url: String,
    /// User-configurable system prompt for note generation.
    pub system_prompt: String,
}

/// Supported AI summarisation providers.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AiProvider {
    /// llama.cpp local inference through llama-cpp-2 bindings.
    #[default]
    #[serde(rename = "llama_cpp", alias = "llama-cpp-2", alias = "llamacpp")]
    LlamaCpp,
    /// OpenAI chat completions.
    Openai,
    /// Anthropic Claude.
    Anthropic,
    /// Local Ollama endpoint.
    Ollama,
}

/// Storage location settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StorageSettings {
    /// Directory containing generated Markdown notes.
    pub notes_dir: String,
    /// SQLite database path.
    pub db_path: String,
}

impl Settings {
    /// Loads settings from the default XDG config path, falling back to defaults when absent.
    pub async fn load() -> Result<Self, CoreError> {
        Self::load_from_path(default_config_path()?).await
    }

    /// Loads settings from a TOML file, falling back to defaults for invalid sections.
    pub async fn load_from_path(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref();
        let content = match fs::read_to_string(path).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default().validated());
            }
            Err(source) => {
                return Err(CoreError::ReadSettings {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };

        let parsed = match content.parse::<toml::Value>() {
            Ok(value) => value,
            Err(error) => {
                warn!(%error, path = %path.display(), "settings TOML is invalid; using defaults");
                return Ok(Self::default().validated());
            }
        };

        Ok(Self::from_value(parsed).validated())
    }

    /// Saves settings to the default XDG config path.
    pub async fn save(&self) -> Result<(), CoreError> {
        self.save_to_path(default_config_path()?).await
    }

    /// Saves settings to a TOML file, creating parent directories when needed.
    pub async fn save_to_path(&self, path: impl AsRef<Path>) -> Result<(), CoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|source| CoreError::WriteSettings {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }

        let content = toml::to_string_pretty(&self.validated())?;
        secure_write(path, content.as_bytes()).await
    }

    /// Returns a settings copy with invalid numeric and empty-string values replaced by defaults.
    #[must_use]
    pub fn validated(&self) -> Self {
        let defaults = Self::default();
        let mut settings = self.clone();

        if settings.audio.input_device.trim().is_empty() {
            warn!("audio.input_device is empty; using default");
            settings.audio.input_device = defaults.audio.input_device;
        }
        if settings.audio.max_recording_size_mb == 0 {
            warn!("audio.max_recording_size_mb is zero; using default");
            settings.audio.max_recording_size_mb = defaults.audio.max_recording_size_mb;
        }
        if settings.audio.chunk_duration_minutes == 0 {
            warn!("audio.chunk_duration_minutes is zero; using default");
            settings.audio.chunk_duration_minutes = defaults.audio.chunk_duration_minutes;
        }
        if settings.transcription.model_path.trim().is_empty() {
            warn!("transcription.model_path is empty; using default");
            settings.transcription.model_path = defaults.transcription.model_path;
        }
        if settings.ai.model.trim().is_empty() {
            warn!("ai.model is empty; using default");
            settings.ai.model = defaults.ai.model;
        }
        if settings.ai.ollama_url.trim().is_empty() {
            warn!("ai.ollama_url is empty; using default");
            settings.ai.ollama_url = defaults.ai.ollama_url;
        }
        if settings.ai.system_prompt.trim().is_empty() {
            warn!("ai.system_prompt is empty; using default");
            settings.ai.system_prompt = defaults.ai.system_prompt;
        }
        if settings.storage.notes_dir.trim().is_empty() {
            warn!("storage.notes_dir is empty; using default");
            settings.storage.notes_dir = defaults.storage.notes_dir;
        }
        if settings.storage.db_path.trim().is_empty() {
            warn!("storage.db_path is empty; using default");
            settings.storage.db_path = defaults.storage.db_path;
        }

        settings
    }

    fn from_value(value: toml::Value) -> Self {
        Self {
            general: section_or_default(&value, "general"),
            meeting: section_or_default(&value, "meeting"),
            audio: section_or_default(&value, "audio"),
            transcription: section_or_default(&value, "transcription"),
            ai: section_or_default(&value, "ai"),
            storage: section_or_default(&value, "storage"),
        }
    }
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            start_on_login: false,
            start_on_login_method: LoginMethod::Xdg,
        }
    }
}

impl Default for MeetingSettings {
    fn default() -> Self {
        Self {
            auto_detect: true,
            auto_capture: true,
            detection_method: RecordingDetectionMethod::Hyprland,
        }
    }
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            input_device: "default".to_owned(),
            capture_loopback: false,
            max_recording_size_mb: 2048,
            chunk_duration_minutes: 10,
        }
    }
}

impl Default for TranscriptionSettings {
    fn default() -> Self {
        Self {
            method: TranscriptionMethod::Local,
            model_path: DEFAULT_TRANSCRIPTION_MODEL_PATH.to_owned(),
            openai_api_key: String::new(),
        }
    }
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            provider: AiProvider::LlamaCpp,
            model: "Qwen/Qwen2.5-3B-Instruct-GGUF".to_owned(),
            model_path: String::new(),
            hf_repo: "Qwen/Qwen2.5-3B-Instruct-GGUF".to_owned(),
            hf_model_file: "qwen2.5-3b-instruct-q4_k_m.gguf".to_owned(),
            api_key: String::new(),
            ollama_url: "http://localhost:11434".to_owned(),
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_owned(),
        }
    }
}

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            notes_dir: DEFAULT_NOTES_DIR.to_owned(),
            db_path: DEFAULT_DB_PATH.to_owned(),
        }
    }
}

/// Returns the default Rustle TOML settings path under XDG config directories.
pub fn default_config_path() -> Result<PathBuf, CoreError> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or(CoreError::MissingDirectory {
            purpose: "settings",
        })?;

    Ok(base.join(APP_CONFIG_DIR).join(CONFIG_FILE_NAME))
}

fn section_or_default<T>(value: &toml::Value, key: &'static str) -> T
where
    T: Default + serde::de::DeserializeOwned,
{
    match value.get(key).cloned() {
        Some(section) => match section.try_into::<T>() {
            Ok(parsed) => parsed,
            Err(error) => {
                warn!(%error, section = key, "settings section is invalid; using defaults");
                T::default()
            }
        },
        None => T::default(),
    }
}

async fn secure_write(path: &Path, content: &[u8]) -> Result<(), CoreError> {
    set_owner_only_permissions(path).await?;

    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .await
        .map_err(|source| CoreError::WriteSettings {
            path: path.to_path_buf(),
            source,
        })?;

    file.write_all(content)
        .await
        .map_err(|source| CoreError::WriteSettings {
            path: path.to_path_buf(),
            source,
        })?;
    file.flush()
        .await
        .map_err(|source| CoreError::WriteSettings {
            path: path.to_path_buf(),
            source,
        })?;

    set_owner_only_permissions(path).await
}

#[cfg(target_family = "unix")]
async fn set_owner_only_permissions(path: &Path) -> Result<(), CoreError> {
    use std::os::unix::fs::PermissionsExt;

    match fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(CoreError::WriteSettings {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_match_spec_values() {
        let settings = Settings::default();

        assert!(settings.meeting.auto_detect);
        assert!(settings.meeting.auto_capture);
        assert_eq!(settings.audio.max_recording_size_mb, 2048);
        assert_eq!(settings.audio.chunk_duration_minutes, 10);
        assert_eq!(settings.ai.provider, AiProvider::LlamaCpp);
        assert_eq!(settings.ai.model, "Qwen/Qwen2.5-3B-Instruct-GGUF");
    }

    #[test]
    fn validation_replaces_invalid_numbers_and_empty_paths() {
        let mut settings = Settings::default();
        settings.audio.max_recording_size_mb = 0;
        settings.audio.chunk_duration_minutes = 0;
        settings.storage.notes_dir = String::new();

        let validated = settings.validated();

        assert_eq!(validated.audio.max_recording_size_mb, 2048);
        assert_eq!(validated.audio.chunk_duration_minutes, 10);
        assert_eq!(validated.storage.notes_dir, DEFAULT_NOTES_DIR);
    }

    #[test]
    fn invalid_section_falls_back_to_defaults() {
        let value = "[meeting]
detection_method = 'invalid'
[audio]
input_device = 'pipewire'
"
        .parse::<toml::Value>()
        .expect("test TOML should parse");

        let settings = Settings::from_value(value);

        assert_eq!(settings.meeting, MeetingSettings::default());
        assert_eq!(settings.audio.input_device, "pipewire");
    }

    #[tokio::test]
    async fn save_creates_settings_file_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir()
            .join("rustle-settings-permissions")
            .join(format!("{}.toml", uuid::Uuid::new_v4()));

        Settings::default()
            .save_to_path(&path)
            .await
            .expect("settings should save");

        let mode = std::fs::metadata(&path)
            .expect("settings file should exist")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(mode, 0o600);

        std::fs::remove_file(&path).expect("settings file should be removed");
        if let Some(parent) = path.parent() {
            std::fs::remove_dir(parent).expect("settings temp dir should be removed");
        }
    }

    #[tokio::test]
    async fn save_restricts_existing_settings_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir()
            .join("rustle-settings-existing-permissions")
            .join(format!("{}.toml", uuid::Uuid::new_v4()));
        let parent = path.parent().expect("path should have parent");
        std::fs::create_dir_all(parent).expect("settings temp dir should be created");
        std::fs::write(&path, "openai_api_key = 'secret'").expect("settings file should be seeded");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("settings file permissions should be widened");

        Settings::default()
            .save_to_path(&path)
            .await
            .expect("settings should save");

        let mode = std::fs::metadata(&path)
            .expect("settings file should exist")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(mode, 0o600);

        std::fs::remove_file(&path).expect("settings file should be removed");
        std::fs::remove_dir(parent).expect("settings temp dir should be removed");
    }
}
