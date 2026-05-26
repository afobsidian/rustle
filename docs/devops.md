# DevOps procedures

Rustle uses the repository `Makefile` as the single entry point for local and CI procedures.

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

## Pipelines

### CI

`.github/workflows/ci.yml` runs on pull requests and pushes to `main` or `copilot/**` branches. It validates the workspace on Rust `1.88.0` and stable using `make fmt`, `make lint`, `make test`, and `make build`.

### Release

`.github/workflows/release.yml` runs for tags matching `v*.*.*` or manually with an existing tag. It validates the release candidate with `make ci`, packages the binary with `make dist`, uploads workflow artifacts, and publishes or updates a GitHub release.

## Specification test traceability

Tests should cite the relevant spec identifier from `docs/specification.md` in the test name. Current automated coverage focuses on implemented foundations:

| Spec                           | Automated coverage                                                                                                                                 |
| ------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| SPEC-009 · Persistent Settings | `crates/rustle-core/tests/spec_009_settings.rs` validates defaults, TOML round trips, invalid-value fallback, and owner-only settings permissions. |
| SPEC-012 · Fault Tolerance     | `crates/rustle-core/tests/spec_012_event_bus.rs` validates event bus delivery and no-receiver error reporting.                                     |

Future feature work should add matching `spec_<id>_*.rs` tests with mocks for DBus, Hyprland IPC, PipeWire, transcription, AI, and storage behaviors described in `docs/specification.md`.
