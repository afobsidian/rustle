# DevOps procedures

Rustle uses the repository `Makefile` as the single entry point for local and CI procedures.

## Local run

Run the manual notes MVP from the repository root:

```sh
make run
```

The current MVP uses terminal commands while the tray implementation is still pending:

| Command                | Purpose                                      |
| ---------------------- | -------------------------------------------- |
| `start [meeting name]` | Starts a manual meeting and opens a draft.   |
| `stop`                 | Reads the draft, generates notes, and saves. |
| `open`                 | Opens the latest saved note or notes folder. |
| `quit`                 | Requests graceful application shutdown.      |

The default local AI provider is the llama.cpp backend using a quantized GGUF model. The first run may download model files, and failures still fall back to local Markdown notes derived from the transcript.

To point Rustle at a different llama.cpp model, set this in `~/.config/rustle/config.toml`:

```toml
[ai]
provider = "llama_cpp"
model = "Qwen/Qwen2.5-0.5B-Instruct-GGUF"
hf_repo = "Qwen/Qwen2.5-0.5B-Instruct-GGUF"
hf_model_file = "qwen2.5-0.5b-instruct-q4_k_m.gguf"
```

Ollama remains available as an optional provider with `provider = "ollama"`. If the configured AI provider fails, Rustle writes fallback notes containing the transcript. A dev container may not expose a graphical session or SNI tray host, so validate desktop tray behavior from the Fedora Hyprland session once the tray backend is implemented.

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
6. Enter `open` to open the latest note or notes folder.
7. Enter `quit` and confirm the process exits.

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

Tests should cite the relevant `SPECS.md` identifier in the test name. Current automated coverage focuses on implemented foundations:

| Spec                           | Automated coverage                                                                                                                                 |
| ------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| SPEC-009 · Persistent Settings | `crates/rustle-core/tests/spec_009_settings.rs` validates defaults, TOML round trips, invalid-value fallback, and owner-only settings permissions. |
| SPEC-012 · Fault Tolerance     | `crates/rustle-core/tests/spec_012_event_bus.rs` validates event bus delivery and no-receiver error reporting.                                     |

Future feature work should add matching `spec_<id>_*.rs` tests with mocks for DBus, Hyprland IPC, PipeWire, transcription, AI, and storage behaviors described in `SPECS.md`.
