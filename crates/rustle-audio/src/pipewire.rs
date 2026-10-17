use std::time::Duration;

use tokio::process::{Child, Command};

use crate::stop_child;

/// Session-owned graph: microphone + output monitor -> private sink/source -> recorder.
/// Every node is owned by a child process; no persistent audio configuration is changed.
pub(crate) struct MixGraph {
    bridge: Child,
    feeds: Vec<Child>,
    sink: String,
    pub(crate) source: String,
    devices: (String, String),
    device_query_failures: u8,
}

pub async fn resolve_devices(input: &str, output: &str) -> Result<(String, String), String> {
    async fn resolve(value: &str, kind: &str) -> Result<String, String> {
        let value = value.trim();
        if !value.is_empty() && !value.eq_ignore_ascii_case("default") {
            let output = tokio::time::timeout(
                Duration::from_secs(3),
                Command::new("pactl")
                    .args(["--format=json", "list", &format!("{kind}s")])
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .map_err(|_| "audio device query timed out".to_owned())?
            .map_err(|e| e.to_string())?;
            let devices: serde_json::Value =
                serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
            return devices.as_array().and_then(|devices| devices.iter().find(|device| {
                device["name"].as_str() == Some(value) || device["properties"]["object.serial"].as_str() == Some(value)
            })).and_then(|device| device["name"].as_str()).map(str::to_owned)
                .ok_or_else(|| format!("configured audio {kind} '{value}' is unavailable; select an existing PipeWire device or default"));
        }
        let output = tokio::time::timeout(
            Duration::from_secs(3),
            Command::new("pactl")
                .arg(format!("get-default-{kind}"))
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| "audio device query timed out".to_owned())?
        .map_err(|e| format!("install pulseaudio-utils (Fedora) or libpulse (Arch): {e}"))?;
        let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || name.is_empty() {
            return Err(format!(
                "no default audio {kind}: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(name)
    }
    let (input, output) = tokio::try_join!(resolve(input, "source"), resolve(output, "sink"))?;
    Ok((input, output))
}

fn spawn(args: &[String]) -> Result<Child, String> {
    Command::new("pw-loopback")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to create PipeWire mixing graph: {e}"))
}

impl MixGraph {
    pub(crate) async fn start(input: &str, output: &str) -> Result<Self, String> {
        let devices = resolve_devices(input, output).await?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let sink = format!("rustle_mix_{id}");
        let source = format!("rustle_capture_{id}");
        let bridge = spawn(&[
            "--channels=1".to_owned(),
            format!("--capture-props=media.class=Audio/Sink node.name={sink} node.virtual=true priority.session=0"),
            format!("--playback-props=media.class=Audio/Source node.name={source} node.virtual=true priority.session=0"),
        ])?;
        let mut graph = Self {
            bridge,
            feeds: Vec::new(),
            sink,
            source,
            devices,
            device_query_failures: 0,
        };
        // Wait for registration, rather than racing the WirePlumber graph.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if graph
                    .bridge
                    .try_wait()
                    .map_err(|e| e.to_string())?
                    .is_some()
                {
                    return Err("PipeWire mixing bridge exited before registration".to_owned());
                }
                let output = tokio::time::timeout(
                    Duration::from_secs(2),
                    Command::new("pw-dump").kill_on_drop(true).output(),
                )
                .await
                .map_err(|_| "pw-dump timed out".to_owned())?
                .map_err(|e| e.to_string())?;
                if output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .contains(&format!("\"{}\"", graph.source))
                {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .map_err(|_| "PipeWire graph registration timed out".to_owned())??;
        graph.connect()?;
        Ok(graph)
    }

    fn connect(&mut self) -> Result<(), String> {
        for (device, monitor) in [
            (&self.devices.0, self.devices.0.ends_with(".monitor")),
            (&self.devices.1, true),
        ] {
            let device = if monitor {
                device.strip_suffix(".monitor").unwrap_or(device)
            } else {
                device
            };
            let mut args = vec![
                "--channels=1".to_owned(),
                format!("--capture={device}"),
                format!("--playback={}", self.sink),
                "--playback-props=node.passive=true audio.volume=0.5".to_owned(),
            ];
            if monitor {
                args.push("--capture-props=stream.capture.sink=true".to_owned());
            }
            self.feeds.push(spawn(&args)?);
        }
        Ok(())
    }

    pub(crate) async fn refresh(&mut self, input: &str, output: &str) -> Result<(), String> {
        if self.bridge.try_wait().map_err(|e| e.to_string())?.is_some() {
            return Err("PipeWire mixing bridge stopped".to_owned());
        }
        for feed in &mut self.feeds {
            if feed.try_wait().map_err(|e| e.to_string())?.is_some() {
                return Err("PipeWire microphone/output connection stopped".to_owned());
            }
        }
        let devices = match resolve_devices(input, output).await {
            Ok(devices) => {
                self.device_query_failures = 0;
                devices
            }
            Err(error) => {
                self.device_query_failures += 1;
                if self.device_query_failures >= 3 {
                    return Err(error);
                }
                // An observation timeout does not mean the running graph failed.
                tracing::warn!(%error, "audio device query failed; keeping current connections and retrying");
                return Ok(());
            }
        };
        if devices != self.devices {
            for feed in &mut self.feeds {
                stop_child(feed).await?;
            }
            self.feeds.clear();
            self.devices = devices;
            self.connect()?;
        }
        Ok(())
    }

    pub(crate) async fn stop(mut self) {
        for feed in &mut self.feeds {
            let _ = stop_child(feed).await;
        }
        let _ = stop_child(&mut self.bridge).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn running_graph_survives_transient_queries_but_bounds_permanent_failure() {
        let mut graph = MixGraph {
            bridge: Command::new("sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .unwrap(),
            feeds: Vec::new(),
            sink: "unused".into(),
            source: "unused".into(),
            devices: ("original-input".into(), "original-output".into()),
            device_query_failures: 0,
        };
        let absent = format!("rustle-absent-{}", uuid::Uuid::new_v4());
        assert!(graph.refresh(&absent, &absent).await.is_ok());
        assert!(graph.refresh(&absent, &absent).await.is_ok());
        assert!(graph.bridge.try_wait().unwrap().is_none());
        assert_eq!(graph.devices.0, "original-input");
        assert!(graph.refresh(&absent, &absent).await.is_err());
        graph.stop().await;
    }
}
