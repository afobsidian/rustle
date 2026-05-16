# DevOps procedures

Rustle uses the repository `Makefile` as the single entry point for local and CI procedures.

## Local validation

Run the full validation gate before opening a pull request:

```sh
make ci
```

The gate runs:

| Procedure | Command | Purpose |
| --- | --- | --- |
| Format | `make fmt` | Checks Rust formatting with `cargo fmt --all -- --check`. |
| Lint | `make lint` | Runs clippy for the full workspace with warnings denied. |
| Test | `make test` | Runs all unit and integration tests in the workspace. |
| Build | `make build` | Builds the workspace with `RUSTFLAGS="-D warnings"`. |

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

`.github/workflows/ci.yml` runs on pull requests and pushes to `main` or `copilot/**` branches. It validates the workspace on Rust `1.75.0` and stable using `make fmt`, `make lint`, `make test`, and `make build`.

### Release

`.github/workflows/release.yml` runs for tags matching `v*.*.*` or manually with an existing tag. It validates the release candidate with `make ci`, packages the binary with `make dist`, uploads workflow artifacts, and publishes or updates a GitHub release.

## Specification test traceability

Tests should cite the relevant `SPECS.md` identifier in the test name. Current automated coverage focuses on implemented foundations:

| Spec | Automated coverage |
| --- | --- |
| SPEC-009 · Persistent Settings | `crates/rustle-core/tests/spec_009_settings.rs` validates defaults, TOML round trips, invalid-value fallback, and owner-only settings permissions. |
| SPEC-012 · Fault Tolerance | `crates/rustle-core/tests/spec_012_event_bus.rs` validates event bus delivery and no-receiver error reporting. |

Future feature work should add matching `spec_<id>_*.rs` tests with mocks for DBus, Hyprland IPC, PipeWire, transcription, AI, and storage behaviors described in `SPECS.md`.
