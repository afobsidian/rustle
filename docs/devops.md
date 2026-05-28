# DevOps procedures

Rustle uses the repository `Makefile` as the single entry point for local and CI procedures.

Related references:

- [architecture.md](architecture.md) for runtime topology and crate boundaries
- [specification.md](specification.md) for product and release-contract specs

## Local run

Run the tray-first v0.1 workflow from the repository root:

```sh
make run
```

Rustle targets Hyprland and starts as a tray app when an SNI host is available. Terminal commands remain as a fallback control path when the tray host or D-Bus is unavailable:

- `start [meeting name]`: Starts a manual meeting and logs a draft path.
- `stop`: Reads the draft, generates notes, and saves.
- `open`: Opens the current transcript draft during an active meeting when one exists, otherwise the latest saved note, otherwise the notes folder.
- `transcript`: Opens the latest transcript draft when one exists.
- `settings`: Opens the TOML settings file in the configured editor.
- `quit`: Requests graceful application shutdown.

Rustle opens notes, transcript drafts, and the settings file by preferring `$VISUAL`, then `$EDITOR`, then `xdg-open`.

Rustle also reconciles start-on-login integration from `~/.config/rustle/config.toml` on startup:

- `general.start_on_login = true` with `general.start_on_login_method = "xdg"` manages `~/.config/autostart/rustle.desktop`.
- `general.start_on_login = true` with `general.start_on_login_method = "systemd"` manages `~/.config/systemd/user/rustle.service` and the matching `default.target.wants` symlink.
- `general.start_on_login = false` removes both integration paths.

Rustle writes structured diagnostics to stderr and to `~/.local/share/rustle/logs/rustle.log` by default. For release support, capture the relevant log excerpt from that file first; unhandled panics are also reported through the top-level panic hook with the log path in the emitted diagnostic context.

Rustle sends desktop notifications for meeting detection and recording start, saved notes, and transcription or summarisation fallbacks that change what the user should expect. If the desktop notification service is unavailable, Rustle should continue running and log the delivery failure instead of crashing.

The manual flow above is the must-pass release path. On Fedora/Hyprland, also validate the automatic Teams-detection path when a tray host is available. The supported automatic scope for v0.1 is Microsoft Teams desktop windows plus common Teams web titles in Chromium-, Chrome-, and Firefox-based browsers. Calendar, Chat, Calls, Activity, and similar navigation views should not trigger meeting start.

Local transcription uses `whisper-rs`. When the default `transcription.model_path` is used, Rustle downloads the whisper.cpp `ggml-base.en.bin` model on first transcription if it is missing. Custom model paths must point to an existing compatible whisper.cpp `ggml` model.

Audio recorder startup and shutdown are bounded for release use. If a recorder backend launches but fails to produce audio beyond the WAV header within the startup timeout, Rustle logs the backend failure, stops that chunk attempt, and avoids leaving a misleading empty chunk behind.

The settings surface may still show `openai` transcription for planned future support, but OpenAI transcription is not supported in v0.1 and should be validated as a clear fallback path rather than a working feature.

The default local AI provider is the llama.cpp backend using a quantized GGUF model. The first run may download model files, and failures still fall back to local Markdown warning notes.

To point Rustle at a different llama.cpp model, set this in `~/.config/rustle/config.toml`:

```toml
[ai]
provider = "llama_cpp"
model = "Qwen/Qwen2.5-3B-Instruct-GGUF"
hf_repo = "Qwen/Qwen2.5-3B-Instruct-GGUF"
hf_model_file = "qwen2.5-3b-instruct-q4_k_m.gguf"
```

Ollama remains available as an optional provider with `provider = "ollama"`. If the configured AI provider fails, Rustle writes fallback notes with the failure reason and keeps the transcript draft separate. A dev container may not expose a Hyprland session or SNI tray host, so validate desktop tray behavior from a Fedora Hyprland session.

The settings surface may still show hosted `openai` and `anthropic` AI providers for planned future support, but they are not supported in v0.1 and should be validated as explicit fallback behavior, not as release-ready integrations.

Benchmark available providers on the current machine with:

```sh
make bench-ai
```

The benchmark prints warmup time, generation time, output size, status, and a short preview for several suitable GGUF model choices running through the local llama.cpp backend.

Manual smoke test:

1. Run `make run`.
2. Enter `start Planning Sync`.
3. Review, edit, or replace the sample text in the draft under `~/.local/share/rustle/transcripts/` and save it.
4. Enter `stop`.
5. Confirm a Markdown note appears under `~/.local/share/rustle/notes/`.
6. While the meeting is active, use `open` to confirm Rustle opens the current transcript draft when one exists.
7. After `stop`, run `open` again and confirm Rustle opens the latest saved note, or the notes folder when no saved note exists.
8. Enter `quit` and confirm the process exits.

Known WAV transcription test:

1. Prepare a short WAV file containing speech, ideally 16 kHz mono.
2. Run `RUSTLE_TEST_AUDIO_FILE=/path/to/sample.wav make run`.
3. Enter `start Known WAV Test`.
4. Enter `stop`; Rustle skips live mic capture and transcribes the configured WAV.
5. Confirm the transcript draft contains text derived from the WAV and the saved note contains generated meeting notes without embedding the transcript.

Hyprland detection smoke test:

1. Run `make run` inside a Fedora Hyprland session with an SNI host such as Waybar.
2. Join a Microsoft Teams meeting in the web or native client.
3. Confirm the tray tooltip or menu shows the active meeting name and that auto-capture starts when `meeting.auto_capture = true`.
4. Open a non-meeting Teams navigation view such as Calendar or Chat and confirm Rustle does not start a meeting from that window title alone.
5. If possible, restart Hyprland or otherwise interrupt the Hyprland event socket, then confirm Rustle keeps polling for clients and resumes socket-driven detection once Hyprland is back.
6. Leave the meeting and confirm the active meeting state clears.
7. Confirm desktop notifications appear for meeting start or detection, recording start, and saved notes. Force a transcription or summarisation fallback and confirm Rustle shows a failure notification without exiting.

### Fedora + Hyprland smoke matrix

Run the smoke matrix from a real Fedora Hyprland session. If you are working from a Distrobox container, launch the app and any desktop-facing helpers through `distrobox-host-exec` or `host-spawn` so Rustle reaches the host Hyprland socket, tray watcher, browser windows, and notification service.

Host assumptions:

- Hyprland session with an SNI host such as Waybar
- Desktop notifications available (for example `swaync`)
- At least one supported Hyprland client you can title like a Teams meeting window
- A speech WAV fixture for the known-WAV row
- Local Whisper model available at the configured path, or permission to download it on first use

Record the current release-signoff run in this table:

| Scenario | How to run | Pass criteria | Result | Notes |
| -------- | ---------- | ------------- | ------ | ----- |
| Tray startup | Launch Rustle in the host session. | Tray registers successfully and stays running. | `PASS` / `FAIL` / `BLOCKED` | Record missing watcher or icon issues. |
| Manual meeting flow | Start a manual meeting, add transcript content, stop the meeting. | Transcript draft and Markdown notes are both created. | `PASS` / `FAIL` / `BLOCKED` | Note transcript and note paths. |
| Transcript access | During an active meeting, use `transcript` and `open`. | Both open the current transcript draft. | `PASS` / `FAIL` / `BLOCKED` | Record the opened path. |
| Settings access | Use `settings`. | Rustle opens the settings file through `$VISUAL`, `$EDITOR`, or `xdg-open`. | `PASS` / `FAIL` / `BLOCKED` | Record the opened path or failure. |
| Notes access after stop | After a meeting ends, use `open`. | Rustle opens the latest saved note. | `PASS` / `FAIL` / `BLOCKED` | Record the opened note path. |
| Notifications | Trigger meeting start/detection and saved-note events. | Notifications are delivered without crashing Rustle. | `PASS` / `FAIL` / `BLOCKED` | Record notification count or viewer evidence. |
| Shutdown | Use `quit`. | Rustle exits cleanly. | `PASS` / `FAIL` / `BLOCKED` | Record any lingering-process issue. |
| Hyprland auto-detection | Present a Hyprland client titled like a Teams meeting, then close it. | Rustle detects the meeting, creates a transcript draft, and clears the meeting when the window closes. | `PASS` / `FAIL` / `BLOCKED` | Record the client title used. |
| Known-WAV transcription | Run with `RUSTLE_TEST_AUDIO_FILE=/path/to/sample.wav`, then start and stop a manual meeting. | Rustle transcribes the WAV, writes the transcript draft, and saves notes. | `PASS` / `FAIL` / `BLOCKED` | Record the WAV path and a transcript excerpt. |

## Local validation

Run the full validation gate before opening a pull request:

```sh
make ci
```

The gate runs:

| Procedure | Command      | Purpose                                                   |
| --------- | ------------ | --------------------------------------------------------- |
| Format    | `make fmt`   | Checks Rust formatting with `cargo fmt --all -- --check`. |
| Lint      | `make lint`  | Runs clippy for the full workspace with warnings denied.  |
| Test      | `make test`  | Runs all unit and integration tests in the workspace.     |
| Build     | `make build` | Builds the workspace with `RUSTFLAGS="-D warnings"`.      |

## Release builds

Validate install layout before cutting an RC:

```sh
make install
```

By default this installs:

- `rustle` to `~/.local/bin/rustle`
- tray icons to `~/.local/share/rustle/icons/`

Rustle resolves tray icons from its default data directory first (`$XDG_DATA_HOME/rustle/icons`, or `~/.local/share/rustle/icons` when unset), then from shared data directories such as `/usr/local/share/rustle/icons` and `/usr/share/rustle/icons`. The installed icon names must remain:

- `rustle.svg`
- `rustle-recording.svg`

Create an optimized binary locally:

```sh
make release-build
```

Create a distributable Linux tarball and checksum:

```sh
make dist
```

Artifacts are written to `dist/` as:

- `rustle-<version>-<host>.tar.gz`
- `rustle-<version>-<host>.tar.gz.sha256`

Create a release RPM and checksum:

```sh
make rpm
```

Artifacts are written to `dist/` as:

- `rustle-<version>-1.<arch>.rpm`
- `rustle-<version>-1.<arch>.rpm.sha256`

## Release-candidate checklist

The release candidate should only pass when every required item below is green. Any failed item blocks the RC until it is fixed and re-run.

### Packaging gate

| Check | Command | Pass criteria | Fail criteria |
| ----- | ------- | ------------- | ------------- |
| Install layout | `make install` | Binary is executable at the install bin path and both tray icons exist under the install data path. | Build fails, binary is missing or not executable, or either icon is missing. |
| Release binary | `make release-build` | `target/release/rustle` is produced successfully. | Release build fails or the optimized binary is missing. |
| Tarball + checksum | `make dist` | `dist/rustle-<version>-<host>.tar.gz` and `.sha256` are created. | Packaging fails or either artifact is missing. |
| RPM + checksum | `make rpm` | `dist/rustle-<version>-1.<arch>.rpm` and `.sha256` are created. | RPM packaging fails or either artifact is missing. |
| Checksum verification | `sha256sum -c dist/*.sha256` | Every generated release artifact verifies as `OK`. | Any generated checksum does not verify. |

### Startup and post-install usability gate

Run these checks from a Fedora Linux session on Hyprland with an SNI host such as Waybar using the installed binary, not `cargo run`.

| Check | How to validate | Pass criteria | Fail criteria |
| ----- | --------------- | ------------- | ------------- |
| Tray startup | Launch the installed `rustle` binary. | Rustle starts, stays running, and the tray icon resolves from the installed icon directory. | Startup crashes, exits unexpectedly, or shows a missing icon. |
| Manual workflow | Start a manual meeting, then stop it after transcript content is available. | Transcript draft and saved Markdown notes are created successfully. | Manual start/stop fails or no transcript/note output is created. |
| Settings access | Use the tray or terminal fallback `settings` command. | The settings file opens via `$VISUAL`, `$EDITOR`, or `xdg-open`. | Settings cannot be opened. |
| Notes and transcript access | Use `open` and `transcript` during and after a meeting. | Active transcript opens during the meeting; latest note opens after stop. | Wrong target opens or the command fails. |
| Notifications | Trigger meeting start, saved-note, and fallback notifications. | User-visible notifications appear when available; delivery failures are logged without crashing Rustle. | Notifications crash the app or expected user-visible fallbacks are silent. |

### Validated RC preparation flow

The current release-candidate packaging flow was exercised locally with sandboxed install paths to avoid mutating the host environment:

```sh
sandbox="$(mktemp -d)"
INSTALL_BIN_DIR="$sandbox/bin" \
INSTALL_DATA_HOME="$sandbox/share" \
TARGET_DIR=target \
DIST_DIR=dist \
make install
make release-build
make dist
make rpm
sha256sum -c dist/*.sha256
```

Observed results from that run:

- installed binary: `$sandbox/bin/rustle`
- installed tray assets: `$sandbox/share/rustle/icons/rustle.svg`, `$sandbox/share/rustle/icons/rustle-recording.svg`
- release RPM: `dist/rustle-0.1.0-1.x86_64.rpm`
- RPM checksum file: `dist/rustle-0.1.0-1.x86_64.rpm.sha256`
- release tarball: `dist/rustle-0.1.0-x86_64-unknown-linux-gnu.tar.gz`
- tarball checksum file: `dist/rustle-0.1.0-x86_64-unknown-linux-gnu.tar.gz.sha256`
- checksum verification: `dist/rustle-0.1.0-1.x86_64.rpm: OK`, `dist/rustle-0.1.0-x86_64-unknown-linux-gnu.tar.gz: OK`

## Pipelines

### CI

`.github/workflows/ci.yml` runs on pull requests and pushes to `main` or `copilot/**` branches. It validates the workspace on Rust `1.88.0` and stable using `make fmt`, `make lint`, `make test`, and `make build`.

### Release

`.github/workflows/release.yml` runs for tags matching `v*.*.*` or manually with an existing tag. It validates the release candidate with `make ci`, packages the binary with `make dist` and `make rpm`, uploads workflow artifacts, and publishes or updates a GitHub release.

## Specification test traceability

Tests should cite the relevant spec identifier from `docs/specification.md` in the test name. Current automated coverage includes both foundational specs and release-contract hardening:

| Spec | Automated coverage |
| ---- | ------------------ |
| SPEC-006 · Audio Transcription | `crates/rustle-transcription/src/lib.rs` validates fixture handling, default model-path behavior, and unsupported OpenAI fallback messaging. |
| SPEC-009 · Persistent Settings | `crates/rustle-core/tests/spec_009_settings.rs` validates defaults, TOML round trips, invalid-value fallback, legacy coercions, and owner-only settings permissions. |
| SPEC-012 · Fault Tolerance | `crates/rustle-core/tests/spec_012_event_bus.rs` validates event bus delivery and no-receiver error reporting. |
| SPEC-017 · Detection State Transitions | `crates/rustle-detection/src/lib.rs` validates start/end transitions, relevant socket event triggers, and polling fallback when the Hyprland socket closes. |
| SPEC-018 · File-Backed Safety Nets | `crates/rustle-transcription/src/lib.rs`, `crates/rustle-ai/src/lib.rs`, and `crates/rustle-storage/src/lib.rs` validate observable fallbacks, persistence safety, and write/delete failure handling. |
| SPEC-021 · End-to-End Manual Workflow | `tests/spec_021_manual_workflow.rs` validates the deterministic manual workflow across transcription, AI fallback, storage, and notifications. |

Future feature work should add matching `spec_<id>_*.rs` tests or release-contract tests for new DBus, Hyprland IPC, PipeWire, transcription, AI, and storage behavior described in `docs/specification.md`.
