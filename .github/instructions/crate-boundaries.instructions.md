---
description: "Use when changing Rustle crate APIs, moving code between crates, wiring events, or reviewing cross-crate dependencies. Covers crate boundaries, rustle-core contracts, and event bus communication."
name: "Rustle Crate Boundaries"
---

# Rustle Crate Boundaries

- Keep feature crates decoupled from each other; put shared contracts, events, types, and errors in `rustle-core` when they are truly common.
- Prefer `rustle-core`'s broadcast event bus for cross-crate coordination instead of direct dependencies between feature crates.
- Avoid adding dependencies from lower-level crates back into UI, tray, AI, audio, storage, detection, or transcription crates.
- Keep public APIs small, typed, and purpose-specific; avoid exposing implementation details just to simplify one caller.
- When moving a type or event into `rustle-core`, update tests or examples that validate the shared contract.
- Use `tracing` at crate boundaries so cross-crate behavior can be diagnosed without coupling modules together.
