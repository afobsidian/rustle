//! Error types shared by Rustle crates.

use std::path::PathBuf;

use thiserror::Error;

/// Errors produced by shared Rustle infrastructure.
#[derive(Debug, Error)]
pub enum CoreError {
    /// A required XDG or home directory could not be resolved.
    #[error("unable to resolve home or XDG directory for {purpose}")]
    MissingDirectory {
        /// The operation that required the directory.
        purpose: &'static str,
    },

    /// A settings file could not be read.
    #[error("failed to read settings from {path}: {source}")]
    ReadSettings {
        /// Settings file path.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// A settings file could not be written.
    #[error("failed to write settings to {path}: {source}")]
    WriteSettings {
        /// Settings file path.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// A settings file could not be serialized.
    #[error("failed to serialize settings: {0}")]
    SerializeSettings(#[from] toml::ser::Error),
}
