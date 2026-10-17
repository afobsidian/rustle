//! Bounded IPC queries, including recovery from stale login/session environments.
use serde::Deserialize;
use std::time::Duration;
use tokio::process::Command;

#[derive(Deserialize)]
struct Instance {
    instance: String,
    #[serde(default)]
    wl_socket: String,
}

fn select_instance(
    bytes: &[u8],
    signature: Option<&str>,
    display: Option<&str>,
) -> Result<String, String> {
    let instances: Vec<Instance> = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let selected = signature
        .and_then(|signature| instances.iter().find(|item| item.instance == signature))
        .or_else(|| {
            display.and_then(|display| instances.iter().find(|item| item.wl_socket == display))
        })
        .or_else(|| {
            if instances.len() == 1 {
                instances.first()
            } else {
                None
            }
        });
    selected
        .filter(|item| !item.instance.is_empty())
        .map(|item| item.instance.clone())
        .ok_or_else(|| {
            "No unambiguous Hyprland instance; launch Rustle from the intended session".to_owned()
        })
}

async fn run(args: &[&str], signature: Option<&str>) -> Result<Vec<u8>, String> {
    let mut command = Command::new("hyprctl");
    command.args(args).kill_on_drop(true);
    if let Some(signature) = signature {
        command.env("HYPRLAND_INSTANCE_SIGNATURE", signature);
    }
    let output = tokio::time::timeout(Duration::from_secs(3), command.output())
        .await
        .map_err(|_| "hyprctl timed out".to_owned())?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "hyprctl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

pub(super) async fn discover() -> Result<String, String> {
    let bytes = run(&["instances", "-j"], None).await?;
    select_instance(
        &bytes,
        std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().as_deref(),
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
    )
}

pub(super) async fn clients() -> Result<Vec<u8>, String> {
    match run(&["clients", "-j"], None).await {
        Ok(bytes) => Ok(bytes),
        Err(_) => run(&["clients", "-j"], Some(&discover().await?)).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_missing_or_restarted_instance_without_choosing_another_session() {
        let one = br#"[{"instance":"new-session","wl_socket":"wayland-2"}]"#;
        assert_eq!(select_instance(one, None, None).unwrap(), "new-session");
        assert_eq!(
            select_instance(one, Some("old-session"), Some("wayland-1")).unwrap(),
            "new-session"
        );
        let two = br#"[{"instance":"a","wl_socket":"wayland-1"},{"instance":"b","wl_socket":"wayland-2"}]"#;
        assert!(select_instance(two, None, None).is_err());
        assert_eq!(select_instance(two, Some("a"), None).unwrap(), "a");
        assert_eq!(
            select_instance(two, Some("stale"), Some("wayland-2")).unwrap(),
            "b"
        );
        assert!(select_instance(b"[]", None, None).is_err());
        assert!(select_instance(b"invalid", None, None).is_err());
    }
}
