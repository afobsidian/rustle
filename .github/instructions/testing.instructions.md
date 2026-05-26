---
description: "Use when adding or updating Rustle tests, fixing regressions, validating shared contracts, or deciding test coverage for Rust workspace changes. Covers focused Rust tests, async behavior, and project-specific regression coverage."
name: "Rustle Testing Guidelines"
---

# Rustle Testing Guidelines

- Add focused tests for behavior changes, bug fixes, shared contracts, and regressions that would be easy to reintroduce.
- Prefer crate-local tests for implementation behavior and integration tests for public cross-crate behavior.
- Keep tests deterministic and avoid depending on a live Fedora/Hyprland session unless the test is explicitly environment-facing.
- Use Tokio test utilities for async behavior instead of blocking sleeps or timing-sensitive assertions.
- Exercise error paths when changing fallible logic, especially where library code returns errors instead of panicking.
- Keep fixtures small and local to the crate unless multiple crates genuinely share the same test data.
