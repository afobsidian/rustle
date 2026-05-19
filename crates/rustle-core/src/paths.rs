//! Filesystem path helpers shared across Rustle crates.

use std::path::{Path, PathBuf};

use crate::errors::CoreError;
use crate::settings::Settings;

const APP_DATA_DIR: &str = "rustle";

/// Expands a leading `~` in a user-configurable path.
pub fn expand_tilde(path: impl AsRef<str>) -> Result<PathBuf, CoreError> {
    let path = path.as_ref();
    if path == "~" {
        return home_dir().map_err(|_| CoreError::MissingDirectory { purpose: "home" });
    }

    if let Some(stripped) = path.strip_prefix("~/") {
        return Ok(home_dir()
            .map_err(|_| CoreError::MissingDirectory { purpose: "home" })?
            .join(stripped));
    }

    Ok(PathBuf::from(path))
}

/// Returns the default Rustle data directory under XDG data directories.
pub fn default_data_dir() -> Result<PathBuf, CoreError> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .ok_or(CoreError::MissingDirectory { purpose: "data" })?;

    Ok(base.join(APP_DATA_DIR))
}

/// Resolves the configured notes directory, honoring XDG data dirs for the default path.
pub fn resolve_notes_dir(settings: &Settings) -> Result<PathBuf, CoreError> {
    if settings.storage.notes_dir == Settings::default().storage.notes_dir {
        return Ok(default_data_dir()?.join("notes"));
    }

    expand_tilde(&settings.storage.notes_dir)
}

/// Converts a user-visible name into a conservative filename component.
#[must_use]
pub fn safe_filename(name: &str) -> String {
    let mut output = String::with_capacity(name.len());

    for character in name.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            output.push(character);
        } else if character.is_whitespace() || matches!(character, '.' | '/' | '\\' | ':') {
            output.push('_');
        }
    }

    let collapsed = output
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");

    if collapsed.is_empty() {
        "meeting".to_owned()
    } else {
        collapsed
    }
}

/// Returns true when `candidate` is inside `base` after path normalization by joining only.
#[must_use]
pub fn is_safe_child(base: &Path, candidate: &Path) -> bool {
    candidate.starts_with(base)
}

fn home_dir() -> Result<PathBuf, ()> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_filename_removes_path_separators() {
        assert_eq!(
            safe_filename("../Team Sync: Planning"),
            "Team_Sync_Planning"
        );
    }

    #[test]
    fn safe_filename_falls_back_for_empty_names() {
        assert_eq!(safe_filename("../../"), "meeting");
    }

    #[test]
    fn resolve_notes_dir_honors_xdg_data_home_for_defaults() {
        let original = std::env::var_os("XDG_DATA_HOME");
        std::env::set_var("XDG_DATA_HOME", "/tmp/rustle-path-test");

        let resolved = resolve_notes_dir(&Settings::default()).expect("notes dir should resolve");

        assert_eq!(
            resolved,
            PathBuf::from("/tmp/rustle-path-test/rustle/notes")
        );
        match original {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
}
