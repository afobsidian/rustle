# Rustle Specification Document

<!-- markdownlint-disable MD024 -->

> Platform: Linux (Fedora / Hyprland)  
> Language: Rust  
> Purpose: AI-powered meeting notes app with system tray presence and Teams detection

---

## 1. Project Overview

A Rust-native desktop application that sits in the system tray, automatically detects Microsoft Teams meetings, and provides AI-assisted note-taking during those meetings. Built for personal use on a Fedora Linux system running the Hyprland Wayland compositor.

Release scope note: The current release scope is tray-first and file-backed. The supported paths today are the manual meeting workflow plus Hyprland-based Teams detection on Fedora/Hyprland, local Whisper transcription, `llama_cpp` as the default AI provider, editor or desktop-opener based note/transcript/settings access, desktop notifications, start-on-login reconciliation, and structured logging. Native notes/settings windows and SQLite-backed search remain follow-on work.

---

## 2. Architecture Overview

Rustle currently uses a tray-first, event-driven architecture. The root binary initialises diagnostics, start-on-login reconciliation, and a shared `rustle-core` broadcast event bus, then wires the tray, detection, audio, transcription, AI, storage, and UI subsystems around that shared contract.

See [docs/architecture.md](architecture.md) for the full software architecture reference, including:

- startup order and long-lived subsystem loops
- crate boundaries and responsibilities
- the `AppEvent` bus contract
- manual and Hyprland-driven meeting flows
- file-backed persistence layout and external Linux integrations

---

## 3. Specifications

### SPEC-001 · Application Startup

**ID:** SPEC-001  
**Title:** Headless System Tray Launch  
**Priority:** P0

#### Description

The application must start without showing any window. The only initial UI is a tray icon in the system notification area (StatusNotifierItem on Hyprland).

#### Acceptance Criteria

- [ ] App launches and registers a StatusNotifierItem via DBus within 2 seconds
- [ ] No window is created at startup
- [ ] A tray icon (SVG or PNG, ≥22×22px) is visible in Waybar or another Hyprland SNI host
- [ ] App does not crash if no SNI host is running (graceful fallback log)
- [ ] Process is identifiable as `rustle` in `ps aux`

#### Technical Notes

- Use `ksni` crate for StatusNotifierItem DBus registration
- Icon path resolved via XDG data dirs (`~/.local/share/rustle/icons/`)
- Log path resolves under `~/.local/share/rustle/`

---

### SPEC-002 · Autostart on Login

**ID:** SPEC-002  
**Title:** XDG Autostart Integration  
**Priority:** P0

#### Description

The app must support enabling/disabling automatic startup on login via the XDG autostart spec, without requiring systemd user services (though that should be an option).

#### Acceptance Criteria

- [x] `general.start_on_login = true` with `general.start_on_login_method = "xdg"` creates `~/.config/autostart/rustle.desktop`
- [x] Disabling start-on-login removes the managed autostart artifacts
- [x] The XDG desktop file includes `Type=Application`, `Exec=<current rustle executable>`, and `X-GNOME-Autostart-enabled=true`
- [x] `general.start_on_login_method = "systemd"` writes `~/.config/systemd/user/rustle.service` and the matching `default.target.wants` symlink
- [x] Switching methods removes stale artifacts from the other integration path

#### Technical Notes

```ini
# ~/.config/autostart/rustle.desktop
[Desktop Entry]
Type=Application
Version=1.0
Name=Rustle
Comment=Tray-first meeting notes app
Exec=/path/to/rustle
Terminal=false
X-GNOME-Autostart-enabled=true
```

---

### SPEC-003 · Tray Icon Menu

**ID:** SPEC-003  
**Title:** Workflow Tray Menu  
**Priority:** P0

#### Description

Right-clicking the tray icon opens a grouped, workflow-first context menu for meeting control, review/edit work, capture preferences, and provider/configuration.

#### Acceptance Criteria

- [x] Top-level menu groups workflow before configuration: **Current Meeting**, workflow status, **Meeting**, **Review & Edit**, **Capture Preferences**, **Providers & Config**, **Quit**
- [x] "Current Meeting" shows the active meeting name when a meeting is active, otherwise `None`
- [x] Left-click on the tray icon plus the `open` and `notes` terminal commands open the current transcript draft during an active meeting when one exists, otherwise the latest saved note, otherwise the notes folder
- [x] "Review & Edit" exposes current draft, latest note, current work, notes/transcript folder shortcuts, and recent notes/transcripts
- [x] Recent transcript entries support reopen, re-summarise to a new note, and managed deletion
- [ ] Menu renders correctly under Hyprland (Waybar SNI support)
- [ ] All menu items have keyboard-accessible mnemonics

#### Menu Structure

```text
[Rustle Icon]
├── 🎙 Current Meeting: <name or "None">
├── ✏️ Workflow Status: current transcript draft | latest note | notes folder
├── 📁 Meeting
│   ├── Start Manual Meeting
│   └── Stop Meeting
├── 📝 Review & Edit
│   ├── Open Current Draft
│   ├── Open Latest Note
│   ├── Open Current Work
│   ├── Open Notes Folder
│   ├── Open Transcript Folder
│   ├── Recent Notes (...)
│   └── Recent Transcripts (...)
├── 🎛 Capture Preferences
├── ⚙ Providers & Config
└── ✕ Quit
```

`Recent Notes` entries support **Open** and **Delete Permanently**. `Recent Transcripts` entries support **Open**, **Summarise To New Note**, and **Delete Permanently**.

---

### SPEC-004 · Microsoft Teams Meeting Detection

**ID:** SPEC-004  
**Title:** Automatic Teams Meeting Detection  
**Priority:** P0

#### Description

The app must detect when the user joins a Microsoft Teams meeting from a Hyprland session and optionally auto-start recording/note-taking.

#### Acceptance Criteria

- [ ] Detects a Teams window (`teams`, `teams-insiders`, or `teams.microsoft.com`) through Hyprland client inspection and window events
- [ ] Emits an internal `MeetingStarted { name: String, source: DetectionSource }` event within 5 seconds of meeting join
- [ ] Emits `MeetingEnded` event within 10 seconds of meeting leave
- [ ] Detection works for Teams Web (browser) and Teams native app
- [ ] Meeting name extracted where possible (window title parsing via Hyprland IPC socket)

#### Detection Strategy

1. **Hyprland IPC** – query `hyprctl clients` JSON for a window with a title matching `| Microsoft Teams` or `teams.microsoft.com`, then subscribe to Hyprland socket2 events for lifecycle updates

#### Technical Notes

- Hyprland IPC socket: `$HYPRLAND_INSTANCE_SIGNATURE` → `$XDG_RUNTIME_DIR/hypr/$SIG/.socket2.sock`
- Use `tokio::net::UnixStream` to subscribe to Hyprland socket2 events

---

### SPEC-005 · Audio Capture

**ID:** SPEC-005  
**Title:** Meeting Audio Capture  
**Priority:** P1

#### Description

When a meeting is detected (and the user has enabled auto-capture), the app captures audio from the default input device or a selected loopback source.

#### Acceptance Criteria

- [ ] Captures audio from a configurable PipeWire/PulseAudio source (default: default input device)
- [ ] Supports loopback capture (meeting audio output) via virtual sink if configured
- [ ] Audio is buffered to a temp file in `~/.local/share/rustle/recordings/`
- [ ] Capture starts automatically on `MeetingStarted` if `auto_capture = true` in settings
- [ ] Capture can be manually started/stopped from tray menu
- [ ] Recording indicator shown in tray icon (e.g. red dot overlay)
- [ ] Max recording size limit configurable (default: 2GB); older chunks rotated

#### Technical Notes

- Use `pw-record`, `parecord`, or `arecord` selected at runtime for Linux capture
- Store as 16kHz mono WAV (optimal for Whisper transcription)
- Chunk recordings into 10-minute segments for incremental transcription

---

### SPEC-006 · Transcription

**ID:** SPEC-006  
**Title:** Audio Transcription  
**Priority:** P1

#### Description

Captured audio is transcribed to text, either locally via Whisper or via a remote API.

Release scope note: `local` Whisper is the supported transcription path today. The settings surface may still show `openai` so the planned support shape remains visible, but OpenAI transcription is currently unsupported in the release build.

#### Acceptance Criteria

- [ ] Local transcription supported via `whisper-rs` (bundled `ggml` model)
- [ ] The `openai` transcription setting remains visible for future support, but selecting it must return a clear unsupported warning in the current release scope
- [ ] Transcription runs on audio chunks as they complete (streaming-style)
- [ ] Transcription output stored as timestamped transcript draft files under `~/.local/share/rustle/transcripts/` in the current release scope
- [ ] Transcription method configurable: `local` | `openai`
- [ ] Local model path configurable (default: `~/.local/share/rustle/models/ggml-base.en.bin`)
- [ ] Errors during transcription logged and surfaced as tray notification; recording continues

---

### SPEC-007 · AI Note Summarisation

**ID:** SPEC-007  
**Title:** Meeting Note Generation  
**Priority:** P1

#### Description

Transcription text is passed to an LLM to produce structured meeting notes.

Release scope note: `llama_cpp` is the primary supported provider today and `ollama` remains an optional local endpoint path when validated in the target environment. `openai` and `anthropic` may remain visible in settings, but they are currently unsupported in the release build and must fall back with a clear warning.

#### Acceptance Criteria

- [ ] Notes generated at end of meeting (or on demand mid-meeting)
- [ ] Output includes: **Summary**, **Key Decisions**, **Action Items**, **Attendees** (if detectable)
- [ ] AI provider configurable: `llama_cpp` (local llama.cpp) | `openai` (GPT-4o) | `anthropic` (Claude) | `ollama` (local)
- [ ] System prompt configurable by user in settings
- [ ] Notes saved as Markdown to `~/.local/share/rustle/notes/YYYY-MM-DD_<meeting-name>.md`
- [ ] Hosted `openai` and `anthropic` providers remain visible but unsupported in the current release scope and must fall back with a clear warning note
- [ ] SQLite-backed note storage or search remains follow-on work after the current file-backed release

---

### SPEC-008 · Notes UI

**ID:** SPEC-008  
**Title:** Notes Viewer Window  
**Priority:** P1

#### Description

A native window for browsing, searching, and editing past meeting notes.

Release scope note: the current release still opens note files or the notes directory through the user's editor or desktop opener. The native Wayland notes window described here remains follow-on work.

#### Acceptance Criteria

- [ ] Window opens on tray left-click or "Open Notes" menu item
- [ ] Lists all past meetings sorted by date descending
- [ ] Full-text search across all notes
- [ ] Notes rendered as Markdown with syntax highlighting for code blocks
- [ ] Notes editable inline (user can amend AI-generated content)
- [ ] Export note as: plain Markdown file, PDF (via `wkhtmltopdf` or `printpdf`)
- [ ] Window is a standard Wayland toplevel (not a popup/overlay)

#### Technical Notes

- UI framework: `iced` (preferred, pure Rust, Wayland-native) or `egui`
- Markdown rendering: `pulldown-cmark` → HTML → embedded webview, or native renderer

---

### SPEC-009 · Settings

**ID:** SPEC-009  
**Title:** Persistent Settings  
**Priority:** P0

#### Description

All user-configurable options are stored persistently. In the current release scope, the tray opens the TOML settings file in the user's editor; a native settings window remains follow-on work.

#### Acceptance Criteria

- [ ] Settings stored as TOML at `~/.config/rustle/config.toml`
- [ ] Settings entry is accessible from the tray menu; in the current release scope it opens the TOML file in the configured editor
- [ ] All settings have sensible defaults; app works out-of-the-box with no settings changes
- [ ] Settings are validated on load; invalid values fall back to defaults with a warning log

#### Settings Schema

```toml
[general]
start_on_login = false
start_on_login_method = "xdg"  # "xdg" | "systemd"

[meeting]
auto_detect = true
auto_capture = true
detection_method = "hyprland"

[audio]
input_device = "default"
capture_loopback = false
max_recording_size_mb = 2048
chunk_duration_minutes = 10

[transcription]
method = "local"  # "local" | "openai"
model_path = "~/.local/share/rustle/models/ggml-base.en.bin"
openai_api_key = ""

[ai]
provider = "llama_cpp"  # "llama_cpp" | "openai" | "anthropic" | "ollama"
model = "Qwen/Qwen2.5-3B-Instruct-GGUF"
model_path = ""
hf_repo = "Qwen/Qwen2.5-3B-Instruct-GGUF"
hf_model_file = "qwen2.5-3b-instruct-q4_k_m.gguf"
api_key = ""
ollama_url = "http://localhost:11434"
system_prompt = """You are a meeting notes assistant. Produce concise, structured notes with a summary, key decisions, action items, and attendees when available."""

[storage]
notes_dir = "~/.local/share/rustle/notes"
db_path = "~/.local/share/rustle/rustle.db"
```

---

### SPEC-010 · Notifications

**ID:** SPEC-010  
**Title:** Desktop Notifications  
**Priority:** P1

#### Description

The app sends desktop notifications for key events.

#### Acceptance Criteria

- [x] Notification on meeting detected or manually started
- [x] Notification on recording start and saved notes
- [x] Notification on transcription/AI error or fallback
- [x] Notifications sent via `notify-rust` (DBus `org.freedesktop.Notifications`)
- [ ] Notifications respect system Do Not Disturb settings
- [ ] Each notification type can be individually disabled in settings

---

### SPEC-011 · Hyprland-Specific Integration

**ID:** SPEC-011  
**Title:** Hyprland IPC Integration  
**Priority:** P1

#### Description

Deep integration with Hyprland for window detection and workspace awareness.

#### Acceptance Criteria

- [ ] Subscribes to Hyprland socket2 event stream for `openwindow`, `closewindow`, `windowtitle` events
- [ ] Correctly resolves `$HYPRLAND_INSTANCE_SIGNATURE` at runtime
- [ ] Handles Hyprland restart gracefully (reconnects to new socket)
- [ ] Optional: auto-move the Rustle notes window to a specific workspace when a meeting starts (configurable)

---

### SPEC-012 · Error Handling & Resilience

**ID:** SPEC-012  
**Title:** Fault Tolerance  
**Priority:** P1

#### Description

The app must not crash due to transient failures in external services.

#### Acceptance Criteria

- [ ] All external I/O (DBus, PipeWire, Hyprland IPC, API calls) wrapped in retry logic with exponential backoff
- [x] Panic handler installed via `std::panic::set_hook`; panics logged before process exits
- [ ] If transcription fails, raw audio file preserved for manual retry
- [ ] If AI summarisation fails, raw transcript preserved and note marked "summarisation pending"
- [x] Structured logging to stderr and file with `tracing`
- [ ] Log rotation after 10MB

---

### SPEC-017 · Hyprland Detection Resilience

**ID:** SPEC-017  
**Title:** Detection State Transitions  
**Priority:** P1

#### Description

The supported Hyprland detection path must behave like a stable release contract: start a detected meeting once, keep the active title current, and recover when the Hyprland event socket disappears.

#### Acceptance Criteria

- [x] A newly detected Teams meeting publishes `MeetingStarted { source: Hyprland }` exactly once for the active Hyprland client address
- [x] A title change for the same active client updates the in-memory meeting name without publishing a duplicate start event
- [x] When the detected client disappears, Rustle publishes `MeetingEnded` for the active detected meeting
- [x] Relevant socket events such as `windowtitle`, `windowtitlev2`, `activewindowv2`, and `closewindow` trigger a detection refresh
- [x] Socket EOF or disconnect keeps polling available so detection can continue while reconnecting
- [x] Reconnect delay uses bounded backoff rather than tight-loop retries

#### Technical Notes

- Hyprland detection combines `hyprctl clients` snapshots with socket2 event triggers
- Polling remains the recovery path when the event listener is temporarily unavailable
- Navigation-only Teams titles should remain filtered so they do not create false-positive meetings

---

### SPEC-018 · Observable Fallbacks and Persistence Safety

**ID:** SPEC-018  
**Title:** File-Backed Safety Nets  
**Priority:** P1

#### Description

When Rustle cannot complete transcription, summarisation, or storage work normally, it must keep the file-backed workflow observable and safe instead of failing silently.

#### Acceptance Criteria

- [x] If deterministic transcript-fixture loading fails, Rustle emits a fallback transcript segment that explains the failure and where the fixture path pointed
- [x] Transcription failures publish a critical user-visible notification that makes it clear the transcript draft remains available
- [x] Unsupported or failing AI summarisation still produces fallback notes and a warning notification instead of dropping the meeting output
- [x] Note-save failures surface the write error to the caller instead of returning success-shaped output
- [x] Deletion requests outside the managed notes/transcripts directories are rejected with a permission error

#### Technical Notes

- The fallback note path is part of the current release behavior for unsupported hosted providers
- Storage safety relies on canonical-path checks before deleting persisted documents
- These protections are release-critical because v0.1 is intentionally file-backed

---

### SPEC-021 · Manual Workflow Release Contract

**ID:** SPEC-021  
**Title:** End-to-End Manual Workflow  
**Priority:** P0

#### Description

The manual meeting flow is the must-pass release path. Starting and stopping a manual meeting must still produce a usable transcript and note outcome even when Rustle falls back from the preferred AI path.

#### Acceptance Criteria

- [x] `MeetingStarted { source: Manual }` creates a transcript draft under the Rustle transcripts data directory
- [x] `MeetingEnded` for that meeting publishes a final transcription result based on recorded chunks, draft edits, or the configured deterministic test fixture
- [x] The resulting notes are saved under the Rustle notes data directory
- [x] When an unsupported AI provider is configured, Rustle still saves fallback notes and publishes a critical warning notification
- [x] The transcript draft remains separate from the saved Markdown notes

#### Technical Notes

- The release-contract test uses `RUSTLE_TEST_AUDIO_FILE` with a transcript fixture to keep the end-to-end workflow deterministic
- The event chain spans `rustle-transcription`, `rustle-ai`, and `rustle-storage`
- This spec complements the manual smoke path in `docs/devops.md`

---

### SPEC-022 · Teams Chat False-Positive Guard

**ID:** SPEC-022  
**Title:** Teams Chat False-Positive Guard  
**Priority:** P1

#### Description

Teams chat or navigation window titles must not start the meeting workflow even if the window class or title still contains Teams branding.

#### Acceptance Criteria

- [x] Titles such as `Chat | User Name` do not produce a meeting candidate
- [x] Chat titles with a Teams suffix such as `Chat | User Name | Microsoft Teams` still do not publish `MeetingStarted`
- [x] Real meeting titles such as `Sprint Planning | Microsoft Teams` continue to detect normally

#### Technical Notes

- Title filtering runs before `MeetingStarted` publication
- This complements the broader navigation-title filter used for Calendar, Chat, Calls, Activity, and similar views

---

### SPEC-023 · Empty Transcript Guard

**ID:** SPEC-023  
**Title:** Empty Transcript Guard  
**Priority:** P1

#### Description

When a meeting ends with no recorded speech, no deterministic fixture, and no manual transcript text, Rustle should keep the draft but stop before AI note generation.

#### Acceptance Criteria

- [x] `MeetingStarted` still creates a transcript draft for the meeting
- [x] Whitespace-only or empty transcript draft content does not publish `TranscriptionReady`
- [x] No summarisation or note-save path is triggered for that meeting

#### Technical Notes

- Finalisation trims transcript draft content before deciding whether any transcript segments exist
- This protects the file-backed workflow from generating empty notes

---

### SPEC-024 · Final Recording Chunk Ordering

**ID:** SPEC-024  
**Title:** Final Recording Chunk Ordering  
**Priority:** P1

#### Description

Meeting finalisation must not race the recorder. If a meeting ends while recording is still active, Rustle must wait for the final chunk and `RecordingStopped` before publishing transcript output.

#### Acceptance Criteria

- [x] `MeetingEnded` with active recording does not immediately publish `TranscriptionReady`
- [x] A late final `RecordingChunkReady` event is still included in final transcript output
- [x] Finalisation proceeds after `RecordingStopped` clears the recording-active state

#### Technical Notes

- The transcription loop tracks `recording_active` and `meeting_ended` per meeting
- Fallback transcript segments from a failed final chunk still flow through the same finalisation path

---

### SPEC-025 · Current Work Open Routing

**ID:** SPEC-025  
**Title:** Current Work Open Routing  
**Priority:** P1

#### Description

The default "open current work" action should route to the most relevant artifact for the user's current workflow state instead of always opening the same directory or file.

#### Acceptance Criteria

- [x] During an active meeting with a transcript draft, the current-work action opens the current transcript draft
- [x] When no meeting is active and a saved note exists, the current-work action opens the latest saved note
- [x] When no draft or saved note exists, the current-work action falls back to the notes folder
- [x] The same routing applies to tray left-click and the terminal `open` / `notes` commands

#### Technical Notes

- Routing is based on the latest transcript and note paths observed on the event stream
- The selected target also controls whether editor-oriented fallbacks should be attempted after `xdg-open`

---

### SPEC-026 · Review & Edit Workflow Menu

**ID:** SPEC-026  
**Title:** Review & Edit Workflow Menu  
**Priority:** P1

#### Description

The tray's review/edit workflow should surface the current draft, latest note, historical documents, and safe follow-up actions without requiring a separate window.

#### Acceptance Criteria

- [x] The top-level tray menu keeps current meeting and workflow status ahead of configuration controls
- [x] The `Review & Edit` submenu exposes current draft, latest note, current work, notes folder, transcript folder, and recent document submenus
- [x] Saved note entries expose reopen and delete actions
- [x] Saved transcript entries expose reopen, summarise-to-new-note, and delete actions
- [x] When an active meeting has no saved chunk yet, the submenu explains that the transcript draft appears after the first saved chunk

#### Technical Notes

- Menu state is derived from in-memory meeting/doc state plus the configured managed directories
- Transcript re-summarisation republishes the existing transcript through `SummariseTranscriptRequested`

---

### SPEC-027 · Desktop Opener and Settings Bootstrap

**ID:** SPEC-027  
**Title:** Desktop Opener and Settings Bootstrap  
**Priority:** P1

#### Description

Rustle's file-backed workflow should integrate with the desktop opener first, then fall back to editor commands when needed, while also creating a usable settings file on demand.

#### Acceptance Criteria

- [x] Rustle tries `xdg-open` before terminal-editor fallbacks for file-oriented open requests
- [x] When editor fallback is requested, `$VISUAL` takes precedence over `$EDITOR`
- [x] Opening settings creates the default config file when it does not already exist
- [x] If the resolved settings path exists but is not a regular file, Rustle logs a warning and refuses to treat it as an openable settings file

#### Technical Notes

- Open candidates are built as an ordered list of commands for notes, transcripts, settings, and requested paths
- Settings bootstrap is part of the user-visible open-settings workflow, not a separate install step

---

## 4. Out of Scope (v1)

- Multi-user / team sync
- Google Meet / Zoom detection
- Calendar integration
- Mobile companion app
- End-to-end encryption of notes

---

## 5. Test Strategy

| Spec     | Unit / crate-local coverage                    | Integration / release-contract coverage     | Manual                         |
| -------- | ---------------------------------------------- | ------------------------------------------- | ------------------------------ |
| SPEC-001 | Tray registration mock                         | DBus roundtrip test                         | Visual tray check              |
| SPEC-002 | File write/delete                              | Desktop file parse                          | Login smoke test               |
| SPEC-004 | Detection logic unit tests                     | Hyprland socket mock                        | Live Teams call                |
| SPEC-005 | Audio buffer chunking                          | Recorder process handling                   | Record + playback              |
| SPEC-006 | Segment parsing and model-path rules           | Deterministic WAV / transcript-fixture flow | Transcribe sample WAV          |
| SPEC-007 | Prompt construction and fallback-note shaping  | Provider fallback contract                  | Review generated notes         |
| SPEC-009 | TOML parse/validate                            | Config round-trip                           | Settings UI smoke              |
| SPEC-017 | Detection state transitions                    | Socket reconnect / polling fallback         | Hyprland restart smoke         |
| SPEC-018 | Storage safety and fallback notifications      | Cross-crate fallback persistence behavior   | Failure-path review            |
| SPEC-021 | N/A                                            | End-to-end manual workflow contract         | Manual release smoke           |
| SPEC-022 | Detection title filtering                      | False-positive meeting guard                | Teams chat/title review        |
| SPEC-023 | Transcript empty guards                        | Empty transcript no-op contract             | Empty-draft smoke              |
| SPEC-024 | N/A                                            | Final recording chunk ordering contract     | End-meeting while recording    |
| SPEC-025 | Open-target selection helpers                  | N/A                                         | `open` / `notes` routing smoke |
| SPEC-026 | Tray menu grouping and document action menus   | N/A                                         | Review & Edit tray smoke       |
| SPEC-027 | Settings bootstrap and opener command ordering | N/A                                         | Settings/opener smoke          |

---

## 6. Implementation Order (Suggested)

1. **SPEC-009** – Config system (everything depends on it)
2. **SPEC-001** – Tray icon + startup
3. **SPEC-002** – Autostart
4. **SPEC-003** – Tray menu
5. **SPEC-004** – Teams detection (Hyprland IPC first)
6. **SPEC-010** – Notifications
7. **SPEC-005** – Audio capture
8. **SPEC-006** – Transcription
9. **SPEC-007** – AI summarisation
10. **SPEC-008** – Notes UI
11. **SPEC-011** – Hyprland deep integration
12. **SPEC-012** – Hardening pass

Later specs such as **SPEC-017** through **SPEC-027** extend the original roadmap with hardening and workflow-alignment expectations for the current architecture.
