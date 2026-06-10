# Rustle Copilot Instructions

Rustle targets Fedora Linux on Hyprland and uses Rust 2021 with MSRV 1.88. Keep crates decoupled through `rustle-core`'s broadcast event bus, avoid `unwrap()` and `expect()` in library code, use Tokio for async I/O, and use `tracing` for logging.

Current release-critical workflow is tray-first and file-backed: manual meetings, Hyprland Teams detection, transcript drafts, local/fallback note generation, safe managed-document deletion, and deterministic `RUSTLE_TEST_AUDIO_FILE` fixtures for end-to-end tests. When you change user-visible workflow contracts or add/rename `spec_0xx_*` tests, update `README.md`, `docs/architecture.md`, `docs/devops.md`, and `docs/specification.md` in the same change.
