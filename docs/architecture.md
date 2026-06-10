# Rustle software architecture

Rustle is a tray-first, file-backed desktop application for Fedora Linux on Hyprland. The current release architecture is intentionally modular: the top-level binary wires together small crates, and runtime coordination happens through `rustle-core`'s Tokio broadcast event bus instead of direct feature-crate coupling.

## Design goals

- Keep feature crates decoupled and coordinated through shared events.
- Prefer observable failures over hidden fallback behavior.
- Treat Fedora + Hyprland as the supported host environment.
- Keep the release path tray-first and editor/file-backed before adding native windows or a database-backed search layer.

## Runtime topology

```text
                         ┌──────────────────────────┐
                         │        src/main.rs       │
                         │ diagnostics + startup    │
                         └────────────┬─────────────┘
                                      │
                                      ▼
                         ┌──────────────────────────┐
                         │ rustle-core::EventBus    │
                         │ Tokio broadcast channel  │
                         └────────────┬─────────────┘
                                      │
       ┌──────────────┬───────────────┼───────────────┬──────────────┬──────────────┐
       ▼              ▼               ▼               ▼              ▼              ▼
  rustle-tray   rustle-detection  rustle-audio  rustle-transcription  rustle-ai  rustle-storage
       │                                                                            │
       └──────────────────────────────┬─────────────────────────────────────────────┘
                                      ▼
                                 rustle-ui

   app modules in the root crate:
   - diagnostics: tracing subscriber + panic hook
   - notifications: desktop notification delivery
   - autostart: XDG/systemd start-on-login reconciliation
```

## Startup sequence

`src/main.rs` owns process startup:

1. Install diagnostics and the panic hook.
2. Load persisted settings and reconcile start-on-login integration.
3. Warn when the current desktop session is not Hyprland.
4. Create the shared `EventBus`.
5. Initialise each subsystem with a shared sender and its own receiver subscription.
6. Block on `QuitRequested` or `Ctrl-C`.

Subsystem initialisation order is deliberate:

1. `notifications`
2. `rustle-tray`
3. `rustle-detection`
4. `rustle-audio`
5. `rustle-transcription`
6. `rustle-ai`
7. `rustle-storage`
8. `rustle-ui`

Each subsystem usually spawns its own long-lived async loop with `rustle_core::tasks::spawn_logged`, then returns control to the main process.

## Crate and module boundaries

| Component              | Responsibility                                                                                                                                         | Key dependencies / host contracts                                               |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------- |
| `rustle-core`          | Shared settings, paths, errors, types, and the event bus contract.                                                                                     | `tokio::sync::broadcast`, `serde`, XDG path resolution.                         |
| `rustle-tray`          | StatusNotifierItem tray integration and terminal fallback commands. Publishes user intents such as start, stop, open, settings, delete, and summarise. | `ksni`, `rustle-storage` listing helpers.                                       |
| `rustle-detection`     | Hyprland client inspection and socket2 event listening for Teams meeting detection.                                                                    | `hyprctl`, Hyprland socket2, supported browser/window title patterns.           |
| `rustle-audio`         | Recorder backend selection and chunked WAV capture.                                                                                                    | `pw-record`, `parecord`, `arecord`, filesystem-backed recording chunks.         |
| `rustle-transcription` | Transcript draft creation and local Whisper transcription for chunk outputs or deterministic test fixtures.                                            | `whisper-rs`, transcript draft files, optional `RUSTLE_TEST_AUDIO_FILE`.        |
| `rustle-ai`            | Transcript-to-notes summarisation and fallback note generation.                                                                                        | `llama-cpp-2`, optional Ollama, explicit unsupported hosted-provider fallbacks. |
| `rustle-storage`       | Saving notes, listing notes/transcripts, and safe deletion inside managed directories.                                                                 | Markdown note files, transcript files, XDG data paths.                          |
| `rustle-ui`            | Opening notes, transcript drafts, and settings through `xdg-open`, with `$VISUAL` or `$EDITOR` as fallbacks.                                           | Desktop opener/editor process spawning.                                         |
| `src/notifications.rs` | Desktop notification delivery for major workflow events and user-visible fallbacks.                                                                    | `notify-rust`, freedesktop notifications DBus service.                          |
| `src/autostart.rs`     | Reconciles XDG autostart or systemd user-service startup artifacts from settings.                                                                      | `~/.config/autostart`, `~/.config/systemd/user`.                                |
| `src/diagnostics.rs`   | Structured logging to stderr and file plus top-level panic capture.                                                                                    | `tracing`, `tracing-subscriber`, `~/.local/share/rustle/logs/rustle.log`.       |

## Event bus contract

Rustle's main shared contract is `rustle_core::AppEvent`. Instead of calling each other directly, feature crates publish and subscribe to events such as:

- `MeetingStarted` / `MeetingEnded`
- `RecordingStarted` / `RecordingChunkReady` / `RecordingStopped`
- `TranscriptDraftReady` / `TranscriptionReady`
- `SummarisationReady` / `NoteSaved`
- `OpenNotesRequested`, `OpenTranscriptRequested`, `OpenSettingsRequested`, `OpenPathRequested`
- `NotificationRequested`, `SummariseTranscriptRequested`, `DeleteDocumentRequested`, `DocumentDeleted`
- `SettingsChanged`, `QuitRequested`

This keeps feature crates independently testable and lets multiple subscribers react to the same workflow transition without introducing reverse dependencies between crates.

## Primary runtime flows

### Manual meeting flow

1. Tray or terminal fallback publishes `MeetingStarted { source: Manual }`.
2. Transcription creates a transcript draft file and publishes `TranscriptDraftReady`.
3. Audio capture starts automatically only when `meeting.auto_capture = true` and no deterministic test fixture overrides live recording.
4. Audio chunks publish `RecordingChunkReady`.
5. If `MeetingEnded` arrives while recording is still active, transcription marks the meeting as ended and waits for `RecordingStopped`.
6. When recording is complete, transcription finalises the draft, appends deterministic fixture text when configured, and publishes `TranscriptionReady`.
7. Empty drafts are preserved but do not publish `TranscriptionReady` or trigger summarisation.
8. AI summarisation publishes `SummarisationReady`.
9. Storage writes the Markdown note and publishes `NoteSaved`.
10. Notifications and UI subscribers react independently to the saved output.

### Automatic Hyprland detection flow

1. Detection polls `hyprctl clients` and listens to Hyprland socket2 events.
2. When a supported Teams title is found, detection publishes `MeetingStarted { source: Hyprland }`.
3. Detection updates in-memory meeting state when titles change.
4. When the meeting disappears, detection publishes `MeetingEnded`.
5. If the event socket drops, the detector falls back to polling and keeps reconnecting with bounded backoff.

### Open/edit flow

`rustle-ui` tracks the latest transcript and note paths from the event stream. `OpenNotesRequested` resolves to the current transcript draft during an active meeting when one exists, otherwise to the latest saved note, otherwise to the notes directory. `OpenTranscriptRequested` opens the latest transcript draft directly. `SummariseTranscriptRequested` reloads an existing transcript file, refuses empty transcript text, and reuses the normal AI/storage pipeline to save a new note. `DeleteDocumentRequested` flows through `rustle-storage`, which canonicalises the path and rejects deletes outside Rustle-managed notes/transcripts directories. `OpenSettingsRequested` ensures the settings file exists before opening it.

## Persistence model

Rustle is currently file-backed.

| Artifact          | Default path                            | Notes                                      |
| ----------------- | --------------------------------------- | ------------------------------------------ |
| Settings          | `~/.config/rustle/config.toml`          | Saved with owner-only permissions on Unix. |
| Logs              | `~/.local/share/rustle/logs/rustle.log` | Written alongside stderr output.           |
| Recordings        | `~/.local/share/rustle/recordings/`     | Chunked 16 kHz mono WAV files.             |
| Transcript drafts | `~/.local/share/rustle/transcripts/`    | Editable `.txt` drafts.                    |
| Notes             | `~/.local/share/rustle/notes/`          | Saved Markdown meeting notes.              |
| Tray icons        | `~/.local/share/rustle/icons/`          | Resolved before shared system data dirs.   |

The settings schema already includes `storage.db_path`, but SQLite-backed note search and storage remain follow-on work; current release behavior does not persist meeting data into a database.

## External integrations

Rustle depends on host facilities rather than bundling them:

- **Hyprland IPC** for supported automatic meeting detection.
- **StatusNotifierItem / DBus** through `ksni` for tray presence.
- **freedesktop notifications** through `notify-rust`.
- **Linux recorder binaries** (`pw-record`, `parecord`, `arecord`) for audio capture.
- **Local Whisper models** for supported transcription in the current release scope.
- **Local llama.cpp or Ollama** for note generation, with explicit fallback notes when unsupported or unavailable providers are configured.

## Cross-cutting reliability rules

- Unsupported or unavailable desktop features should degrade to logged warnings, terminal fallback controls, or explicit fallback notes instead of panicking.
- Transcript drafts remain available even when transcription or summarisation fails.
- Empty transcript drafts are preserved but do not trigger summarisation.
- Transcription finalisation waits for `RecordingStopped` after `MeetingEnded` so the last chunk is not lost.
- Storage operations validate that deletes stay inside Rustle-managed directories.
- Startup and shutdown of recorder subprocesses are bounded to avoid hanging chunk workers.
- The process installs a panic hook so unhandled panics are still observable in logs.

## Current scope boundaries

Implemented and release-critical today:

- Tray-first startup and controls
- Manual meeting workflow
- Hyprland-based Teams detection
- File-backed transcript drafts and Markdown notes
- Local Whisper transcription
- Local AI generation with explicit fallback notes
- Start-on-login reconciliation
- Desktop notifications and structured logging

Deferred or intentionally follow-on:

- Native notes and settings windows
- SQLite-backed note search/storae
- Hosted OpenAI and Anthropic production integrations
