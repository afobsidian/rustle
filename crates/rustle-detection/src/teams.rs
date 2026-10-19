//! Read-only integration with Teams for Linux 2.25+'s session-bus API.

use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

pub const TEAMS_BUS: &str = "com.github.IsmaelMartinez.teams_for_linux";
const TEAMS_PATH: &str = "/com/github/IsmaelMartinez/teams_for_linux";
const TEAMS_INTERFACE: &str = "com.github.IsmaelMartinez.teams_for_linux.Control";

fn bus_name() -> String {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .and_then(|home| {
            std::fs::read(home.join(format!(
                ".var/app/{TEAMS_BUS}/config/teams-for-linux/config.json"
            )))
            .ok()
        })
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|config| {
            config["dbusControl"]["busName"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| TEAMS_BUS.to_owned())
}

pub(crate) async fn has_owner() -> Result<bool, String> {
    let output = tokio::time::timeout(
        Duration::from_secs(3),
        Command::new("busctl")
            .args([
                "--user",
                "--timeout=2",
                "--json=short",
                "call",
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "NameHasOwner",
                "s",
                &bus_name(),
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "D-Bus owner query timed out".to_owned())?
    .map_err(|e| e.to_string())?;
    let reply: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    reply["data"][0]
        .as_bool()
        .ok_or_else(|| "invalid D-Bus owner reply".to_owned())
}

#[derive(Deserialize)]
struct CallState {
    #[serde(rename = "inCall")]
    in_call: bool,
}

fn parse_state(bytes: &[u8]) -> Result<bool, String> {
    let reply: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let state = reply["data"][0]
        .as_str()
        .ok_or("Teams GetState returned an invalid D-Bus reply")?;
    serde_json::from_str::<CallState>(state)
        .map(|state| state.in_call)
        .map_err(|e| format!("invalid Teams state: {e}"))
}

/// Returns observed call state. An unavailable API is an error, never an idle call.
pub async fn call_state() -> Result<bool, String> {
    let bus = bus_name();
    let output = tokio::time::timeout(
        Duration::from_secs(3),
        Command::new("busctl")
            .args([
                "--user",
                "--timeout=2",
                "--json=short",
                "call",
                &bus,
                TEAMS_PATH,
                TEAMS_INTERFACE,
                "GetState",
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "Teams GetState timed out".to_owned())?
    .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Teams for Linux >=2.25.0 with dbusControl.enabled=true is required: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_state(&output.stdout)
}

pub(crate) async fn watch(trigger: mpsc::UnboundedSender<()>) {
    loop {
        let bus = bus_name();
        let mut child = match Command::new("busctl")
            .args(["--user", "--json=short", "monitor", &bus])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        if let Some(stdout) = child.stdout.take() {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let message = serde_json::from_str::<serde_json::Value>(&line).ok();
                if message
                    .as_ref()
                    .is_some_and(|value| relevant_signal(value, &bus))
                    && trigger.send(()).is_err()
                {
                    return;
                }
            }
        }
        let _ = child.wait().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

fn relevant_signal(message: &serde_json::Value, bus: &str) -> bool {
    match message["member"].as_str() {
        Some("InCallChanged") => true,
        Some("NameOwnerChanged") => message["payload"]["data"][0].as_str() == Some(bus),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_unrelated_dbus_name_changes() {
        let bus = "com.github.IsmaelMartinez.teams_for_linux";
        assert!(!relevant_signal(
            &serde_json::json!({"member":"NameOwnerChanged","payload":{"data":[":1.42","",":1.42"]}}),
            bus
        ));
        assert!(relevant_signal(
            &serde_json::json!({"member":"NameOwnerChanged","payload":{"data":[bus,"",":1.42"]}}),
            bus
        ));
        assert!(relevant_signal(
            &serde_json::json!({"member":"InCallChanged"}),
            bus
        ));
    }

    #[test]
    fn only_in_call_controls_recording() {
        for (state, expected) in [
            (r#"{"inCall":true,"microphoneState":"muted"}"#, true),
            (
                r#"{"inCall":false,"incomingCall":true,"meetingStarted":true}"#,
                false,
            ),
        ] {
            let reply = serde_json::json!({"type":"s", "data":[state]});
            assert_eq!(
                parse_state(&serde_json::to_vec(&reply).unwrap()).unwrap(),
                expected
            );
        }
        assert!(parse_state(br#"{"data":["{}"]}"#).is_err());
        assert!(parse_state(b"not json").is_err());
    }
}
