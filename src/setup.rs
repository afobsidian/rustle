use std::path::{Path, PathBuf};
use std::time::Duration;

use rustle_core::{default_data_dir, resolve_notes_dir, Settings};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

async fn command(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    anyhow::ensure!(
        result.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    Ok(String::from_utf8(result.stdout)?.trim().to_owned())
}

fn supported_version(version: &str) -> bool {
    let mut parts = version.trim_start_matches('v').split('.');
    let major = parts.next().and_then(|v| v.parse::<u64>().ok());
    let minor = parts.next().and_then(|v| v.parse::<u64>().ok());
    matches!((major, minor), (Some(major), Some(minor)) if (major, minor) >= (2, 25))
}

async fn teams_version() -> anyhow::Result<String> {
    let apps = command(
        "flatpak",
        &["list", "--app", "--columns=application,version,branch"],
    )
    .await?;
    for line in apps.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.first() == Some(&rustle_detection::teams::TEAMS_BUS)
            && fields
                .get(1)
                .is_some_and(|version| supported_version(version))
        {
            return Ok(line.to_owned());
        }
    }
    anyhow::bail!("install Teams for Linux >=2.25.0 (Flatpak beta if stable is older); launch the required branch explicitly")
}

fn teams_config_path() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME is unset"))?;
    Ok(PathBuf::from(home).join(
        ".var/app/com.github.IsmaelMartinez.teams_for_linux/config/teams-for-linux/config.json",
    ))
}

async fn enable_teams(path: &Path) -> anyhow::Result<()> {
    let original = match tokio::fs::read(path).await {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut config: Value = match &original {
        Some(bytes) => serde_json::from_slice(bytes)?,
        None => json!({}),
    };
    anyhow::ensure!(
        config.is_object(),
        "Teams config must be a JSON object; existing file preserved"
    );
    if config.get("dbusControl").is_none() {
        config["dbusControl"] = json!({});
    }
    anyhow::ensure!(
        config["dbusControl"].is_object(),
        "Teams dbusControl must be an object; existing file preserved"
    );
    if config["dbusControl"]["enabled"] == true {
        return Ok(());
    }
    config["dbusControl"]["enabled"] = json!(true);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if let Some(bytes) = original {
        use tokio::io::AsyncWriteExt;
        let backup = path.with_extension(format!("{}.backup", uuid::Uuid::new_v4()));
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(backup)
            .await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
    }
    use tokio::io::AsyncWriteExt;
    let pending = path.with_extension(format!("{}.pending", uuid::Uuid::new_v4()));
    let result: anyhow::Result<()> = async {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&pending)
            .await?;
        file.write_all(&serde_json::to_vec_pretty(&config)?).await?;
        file.sync_all().await?;
        tokio::fs::rename(&pending, path).await?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(pending).await;
    }
    result?;
    tracing::warn!("Teams D-Bus enabled; restart Teams for Linux if it is already open");
    Ok(())
}

pub(crate) async fn prepare() -> anyhow::Result<()> {
    crate::models::check_cpu()?;
    let mut settings = Settings::load().await?;
    if settings.meeting.auto_detect {
        teams_version().await?;
        enable_teams(&teams_config_path()?).await?;
    }
    if settings.meeting.auto_capture && settings.audio.capture_loopback {
        rustle_audio::pipewire::resolve_devices(
            &settings.audio.input_device,
            &settings.audio.output_device,
        )
        .await
        .map_err(anyhow::Error::msg)?;
    }
    crate::notifications::setup_notice("Rustle setup", "Downloading and verifying local models. Setup will then test virtual audio, transcription, and notes automatically.").await;
    crate::models::prepare(&mut settings).await?;
    settings.save().await?;
    Ok(())
}

fn fingerprint(settings: &Settings) -> anyhow::Result<String> {
    let mut hash = Sha256::new();
    hash.update(env!("CARGO_PKG_VERSION"));
    hash.update(serde_json::to_vec(settings)?);
    for path in crate::models::paths(settings) {
        let metadata = std::fs::metadata(&path)?;
        hash.update(metadata.len().to_le_bytes());
        hash.update(
            metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
                .to_le_bytes(),
        );
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) async fn is_ready(settings: &Settings) -> bool {
    let Ok(expected) = fingerprint(settings) else {
        return false;
    };
    let Ok(directory) = default_data_dir() else {
        return false;
    };
    tokio::fs::read_to_string(directory.join("ready.sha256"))
        .await
        .is_ok_and(|actual| actual == expected)
}

pub(crate) async fn mark_ready(settings: &Settings) -> anyhow::Result<()> {
    let directory = default_data_dir()?;
    tokio::fs::create_dir_all(&directory).await?;
    tokio::fs::write(directory.join("ready.sha256"), fingerprint(settings)?).await?;
    crate::notifications::setup_notice("Rustle is ready", "The automatic capture-to-note test passed. Teams for Linux must be restarted after enabling its D-Bus integration.").await;
    Ok(())
}

pub(crate) async fn doctor() -> anyhow::Result<()> {
    let settings = Settings::load().await?;
    let mut checks = Vec::new();
    fn check(checks: &mut Vec<Value>, name: &str, result: anyhow::Result<String>) {
        checks.push(match result {
            Ok(detail) => json!({"name":name,"ok":true,"detail":detail}),
            Err(error) => json!({"name":name,"ok":false,"detail":error.to_string()}),
        });
    }
    check(&mut checks, "cpu", crate::models::check_cpu());
    check(
        &mut checks,
        "hyprland",
        async {
            anyhow::ensure!(
                rustle_core::is_hyprland_session(),
                "launch Rustle inside the Hyprland user session"
            );
            command("hyprctl", &["clients", "-j"]).await?;
            Ok("Hyprland IPC accessible".to_owned())
        }
        .await,
    );
    check(&mut checks, "teams_version", teams_version().await);
    check(
        &mut checks,
        "teams_dbus",
        rustle_detection::teams::call_state()
            .await
            .map(|in_call| format!("API accessible; selected profile inCall={in_call}"))
            .map_err(anyhow::Error::msg),
    );
    for program in [
        "pw-record",
        "pw-loopback",
        "pw-dump",
        "pw-play",
        "python3",
        "curl",
    ] {
        check(&mut checks, program, command(program, &["--version"]).await);
    }
    check(
        &mut checks,
        "audio_devices",
        rustle_audio::pipewire::resolve_devices(
            &settings.audio.input_device,
            &settings.audio.output_device,
        )
        .await
        .map(|devices| format!("microphone={}, output={}", devices.0, devices.1))
        .map_err(anyhow::Error::msg),
    );
    check(
        &mut checks,
        "models",
        async {
            let paths = crate::models::paths(&settings);
            anyhow::ensure!(
                !paths.is_empty(),
                "run rustle setup to provision local models"
            );
            for path in paths {
                anyhow::ensure!(
                    std::fs::metadata(&path)?.len() > 0,
                    "model is empty: {}",
                    path.display()
                );
            }
            anyhow::ensure!(
                is_ready(&settings).await,
                "run rustle setup; inference self-test has not passed for this configuration"
            );
            Ok("provisioned and self-test passed".to_owned())
        }
        .await,
    );
    check(
        &mut checks,
        "storage",
        async {
            let directory = resolve_notes_dir(&settings)?;
            tokio::fs::create_dir_all(&directory).await?;
            let probe = directory.join(format!(".rustle-probe-{}", uuid::Uuid::new_v4()));
            let file = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&probe)
                .await?;
            drop(file);
            tokio::fs::remove_file(probe).await?;
            Ok(directory.display().to_string())
        }
        .await,
    );
    let ok = checks.iter().all(|check| check["ok"] == true);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"ok":ok,"checks":checks}))?
    );
    anyhow::ensure!(
        ok,
        "required readiness checks failed; see JSON details above"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn teams_setup_preserves_config_and_rejects_malformed_json() {
        let directory = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        tokio::fs::create_dir(&directory).await.unwrap();
        let path = directory.join("config.json");
        let original = br#"{"theme":"dark","dbusControl":{"busName":"custom.name"}}"#;
        tokio::fs::write(&path, original).await.unwrap();
        enable_teams(&path).await.unwrap();
        let changed = tokio::fs::read(&path).await.unwrap();
        let value: Value = serde_json::from_slice(&changed).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["dbusControl"]["busName"], "custom.name");
        assert_eq!(value["dbusControl"]["enabled"], true);
        enable_teams(&path).await.unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), changed);
        tokio::fs::write(&path, b"broken").await.unwrap();
        assert!(enable_teams(&path).await.is_err());
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"broken");
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
    #[test]
    fn required_teams_version() {
        for version in ["2.25.0", "2.26.0", "3.0.0"] {
            assert!(supported_version(version));
        }
        for version in ["2.24.9", "1.99.0", "unknown", ""] {
            assert!(!supported_version(version));
        }
    }
}
