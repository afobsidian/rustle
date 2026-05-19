# Rustle Copilot Instructions

Rustle targets Fedora Linux on Hyprland/Wayland and uses Rust 2021 with MSRV 1.88. Keep crates decoupled through `rustle-core`'s broadcast event bus, avoid `unwrap()` and `expect()` in library code, use Tokio for async I/O, and use `tracing` for logging.
