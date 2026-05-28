# Rustle

Rust-native desktop meeting notes app for Fedora Linux on Hyprland.

## Specification

See [docs/specification.md](docs/specification.md) for the product and technical specification.

## Run Rustle

Rustle starts as a StatusNotifierItem system tray app on Hyprland with an SNI host such as Waybar. It keeps terminal controls as a fallback when a tray host or D-Bus is unavailable, warns when launched outside Hyprland, starts audio capture for meetings when `meeting.auto_capture = true`, transcribes completed WAV chunks through the configured local Whisper model, summarizes the transcript through the configured AI provider, and writes Markdown notes.

```sh
make run
```

Tray actions are grouped around the user workflow: meeting controls, review and editing, capture preferences, and provider/configuration. Left-clicking the tray icon and the `open` command now follow the same rule: open the current transcript draft during an active meeting when one exists, otherwise open the latest saved note, otherwise open the notes folder.

Available fallback terminal commands while Rustle is running:

```text
start [meeting name]
stop
open
transcript
settings
quit
help
```

The manual meeting flow is the must-pass release workflow. On Fedora/Hyprland, Rustle also ships Hyprland-based Teams detection as the supported automatic path for Microsoft Teams desktop windows and common Teams web titles in browsers such as Chromium, Chrome, and Firefox. Navigation views such as Calendar, Chat, Calls, and Activity are intentionally ignored so they do not start meetings by mistake.

The audio recorder uses the first available Linux recording tool in this order: `pw-record` (PipeWire), `parecord` (PulseAudio), then `arecord` (ALSA). On Fedora/PipeWire systems this should work with the OS-provided PipeWire tools; no extra Rustle-specific service is required. Recordings are saved as 16 kHz mono WAV chunks under `~/.local/share/rustle/recordings/`.

Recorder startup and shutdown are now bounded for release reliability. If a selected backend launches but does not produce audio bytes within a short startup window, Rustle stops that chunk attempt, logs the backend failure, and avoids publishing an empty chunk downstream.

To test transcription with a known WAV instead of the live mic, run Rustle with `RUSTLE_TEST_AUDIO_FILE=/path/to/sample.wav`. Live audio capture is skipped for that run, and the WAV is transcribed when you enter `stop` for the manual meeting.

Rustle opens notes, transcript drafts, and the settings file by preferring `$VISUAL`, then `$EDITOR`, then `xdg-open`. This keeps editing predictable while the app remains tray-first.

Local transcription uses `whisper-rs` and reads the model configured by `transcription.model_path`. When the default model path is used, Rustle downloads the whisper.cpp `ggml-base.en.bin` model on first transcription if it is missing:

```toml
[transcription]
method = "local"
model_path = "~/.local/share/rustle/models/ggml-base.en.bin"
```

For a custom `transcription.model_path`, download or place a compatible whisper.cpp `ggml` model at that path yourself.

The settings surface still shows `openai` as a transcription option so the planned support shape stays visible, but OpenAI transcription is not supported in v0.1 yet. If selected, Rustle logs a clear warning and instructs you to use local Whisper instead.

The default AI provider is the local `llama_cpp` backend using a quantized GGUF model. The first run may download model files, and failures still fall back to locally generated Markdown warning notes.

To point Rustle at a different local llama.cpp model, set this in `~/.config/rustle/config.toml`:

```toml
[ai]
provider = "llama_cpp"
model = "Qwen/Qwen2.5-3B-Instruct-GGUF"
hf_repo = "Qwen/Qwen2.5-3B-Instruct-GGUF"
hf_model_file = "qwen2.5-3b-instruct-q4_k_m.gguf"
```

Ollama remains available as an optional provider by setting `provider = "ollama"`. The settings surface also keeps `openai` and `anthropic` visible, but those hosted providers are not supported in v0.1 yet. If a hosted provider is selected, Rustle saves fallback notes with the reason instead of failing silently. Transcript drafts are created under `~/.local/share/rustle/transcripts/` so you can monitor or manually edit transcript text, and Markdown notes are saved under `~/.local/share/rustle/notes/` without embedding the transcript. Settings are stored at `~/.config/rustle/config.toml` and can be opened from the tray menu.

Rustle now honors the existing start-on-login settings on Linux. Set `general.start_on_login = true` and choose `general.start_on_login_method = "xdg"` to manage `~/.config/autostart/rustle.desktop`, or `"systemd"` to manage `~/.config/systemd/user/rustle.service` plus its `default.target.wants` symlink. Switching methods cleans up the stale integration path on the next launch.

Rustle now writes structured diagnostics to stderr and to `~/.local/share/rustle/logs/rustle.log` by default. Unhandled panics are captured through a top-level panic hook so release-support triage has a stable log destination.

Rustle also sends desktop notifications for meeting detection and recording start, saved notes, and user-visible transcription or summarisation fallbacks. If desktop notification delivery is unavailable, Rustle logs the failure and continues running.

Run `make bench-ai` to compare several suitable GGUF models through the local llama.cpp backend on this machine.

Hyprland-based Teams detection is implemented for the supported automatic workflow on Fedora/Hyprland. Rustle keeps polling Hyprland clients even when the event socket is temporarily unavailable, and reconnects to the socket automatically after Hyprland restarts so detection can recover without restarting the app. Native notes and settings windows remain follow-on work; v0.1 stays tray-first and opens notes, transcripts, and settings through your editor or desktop opener.

## DevOps

Use `make ci` to run the local validation gate. See [docs/devops.md](docs/devops.md) for test, build, release, and pipeline procedures, including the release-candidate checklist for `make install`, `make release-build`, `make dist`, checksum verification, post-install startup checks, and the Fedora/Hyprland smoke matrix.
