---
description: "Use when editing Rustle code, refactoring crates, reviewing Rust implementation choices, or deciding project-specific engineering tradeoffs. Covers Rustle architecture, async, logging, and error-handling principles."
name: "Rustle Engineering Principles"
---

# Rustle Engineering Principles

- Target Fedora Linux on Hyprland, Rust 2021, and the workspace MSRV when choosing APIs or platform assumptions.
- Keep crates decoupled. Prefer communication through `rustle-core`'s broadcast event bus over direct cross-crate coupling.
- Preserve existing crate boundaries, naming, module structure, and dependency choices before introducing new abstractions.
- Avoid `unwrap()` and `expect()` in library code. Return meaningful errors and keep failures observable.
- Use Tokio for async I/O and `tracing` for logs, spans, and diagnostic events.
- Keep changes focused on the requested behavior, and call out nearby issues instead of silently expanding scope.
- Add or update focused tests when behavior changes, shared contracts move, or regressions would be easy to miss.
