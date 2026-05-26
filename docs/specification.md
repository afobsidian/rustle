# Rustle Specification Document

<!-- markdownlint-disable MD024 -->

> Platform: Linux (Fedora / Hyprland)  
> Language: Rust  
> Purpose: AI-powered meeting notes app with system tray presence and Teams detection

---

## 1. Project Overview

A Rust-native desktop application that sits in the system tray, automatically detects Microsoft Teams meetings, and provides AI-assisted note-taking during those meetings. Built for personal use on a Fedora Linux system running the Hyprland Wayland compositor.

Release scope note: Rustle v0.1 is tray-first and file-backed. The supported paths today are the manual meeting workflow plus Hyprland-based Teams detection on Fedora/Hyprland, local Whisper transcription, `llama_cpp` as the default AI provider, and editor or desktop-opener based note, transcript, and settings access. Native notes/settings windows, desktop notifications, autostart integration, and SQLite-backed search remain follow-on work.

---

## 2. Architecture Overview

```text
┌─────────────────────────────────────────────────┐
│                    App Core                     │
│  ┌─────────────┐  ┌──────────┐  ┌───────────┐  │
│  │ Tray Manager│  │ Settings │  │ Note Store│  │
│  └─────────────┘  └──────────┘  └───────────┘  │
│  ┌─────────────┐  ┌──────────┐  ┌───────────┐  │
│  │Meeting Detect│  │ AI Client│  │Audio Capt.│  │
│  └─────────────┘  └──────────┘  └───────────┘  │
└─────────────────────────────────────────────────┘
```

**Key crates and runtime components (current v0.1):**

- `ksni` – StatusNotifierItem tray integration on Linux desktop environments with SNI support
- `serde` / `serde_json` / `toml` – config serialisation and Hyprland JSON parsing
- `tokio` – async runtime, process execution, Unix sockets, and task orchestration
- `whisper-rs` – local Whisper transcription
- `llama-cpp-2` – in-process local AI note generation
- `tracing` / `tracing-subscriber` – diagnostics and logging
- External recorder binaries (`pw-record`, `parecord`, `arecord`) – audio capture selected at runtime
- Hyprland IPC (`hyprctl` and socket2) – meeting detection and window lifecycle events

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
- Log path should resolve under `~/.local/share/rustle/` when file logging is introduced

---

### SPEC-002 · Autostart on Login

**ID:** SPEC-002  
**Title:** XDG Autostart Integration  
**Priority:** P0

#### Description

The app must support enabling/disabling automatic startup on login via the XDG autostart spec, without requiring systemd user services (though that should be an option).

#### Acceptance Criteria

- [ ] Settings toggle "Start on login" creates `~/.config/autostart/rustle.desktop` when enabled
- [ ] Toggling off removes the `.desktop` file
- [ ] The `.desktop` file correctly uses `Exec=rustle --tray` and `X-GNOME-Autostart-enabled=true`
- [ ] A secondary option "Use systemd user service" installs/enables `~/.config/systemd/user/rustle.service`
- [ ] Both methods are mutually exclusive in settings

#### Technical Notes

```ini
# ~/.config/autostart/rustle.desktop
[Desktop Entry]
Type=Application
Name=Rustle
Exec=/usr/local/bin/rustle --tray
Hidden=false
X-GNOME-Autostart-enabled=true
```

---

### SPEC-003 · Tray Icon Menu

**ID:** SPEC-003  
**Title:** Tray Context Menu  
**Priority:** P0

#### Description

Right-clicking the tray icon opens a context menu with core actions.

#### Acceptance Criteria

- [x] Menu contains: **Open Notes**, **Current Meeting** (greyed out if none), **Settings**, **Quit**
- [ ] "Current Meeting" shows active meeting name when a Teams meeting is detected
- [x] Left-click on tray icon opens the latest saved note, or the notes folder when no notes exist yet
- [ ] Menu renders correctly under Hyprland (Waybar SNI support)
- [ ] All menu items have keyboard-accessible mnemonics

#### Menu Structure

```text
[Rustle Icon]
├── 📋 Open Notes
├── 🎙 Current Meeting: <name or greyed "None">
├── ──────────────
├── ⚙  Settings
└── ✕  Quit
```

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

Release scope note: for v0.1, `local` Whisper is the supported transcription path. The settings surface may still show `openai` so the planned support shape remains visible, but OpenAI transcription is not yet supported in the release build.

#### Acceptance Criteria

- [ ] Local transcription supported via `whisper-rs` (bundled `ggml` model)
- [ ] The `openai` transcription setting remains visible for future support, but selecting it must return a clear unsupported warning in v0.1
- [ ] Transcription runs on audio chunks as they complete (streaming-style)
- [ ] Transcription output stored as timestamped transcript draft files under `~/.local/share/rustle/transcripts/` in v0.1
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

Release scope note: for v0.1, `llama_cpp` is the primary supported provider and `ollama` remains an optional local endpoint path when validated in the target environment. `openai` and `anthropic` may remain visible in settings, but they are not yet supported in the release build and must fall back with a clear warning.

#### Acceptance Criteria

- [ ] Notes generated at end of meeting (or on demand mid-meeting)
- [ ] Output includes: **Summary**, **Key Decisions**, **Action Items**, **Attendees** (if detectable)
- [ ] AI provider configurable: `llama_cpp` (local llama.cpp) | `openai` (GPT-4o) | `anthropic` (Claude) | `ollama` (local)
- [ ] System prompt configurable by user in settings
- [ ] Notes saved as Markdown to `~/.local/share/rustle/notes/YYYY-MM-DD_<meeting-name>.md`
- [ ] Hosted `openai` and `anthropic` providers remain visible but unsupported in v0.1 and must fall back with a clear warning note
- [ ] SQLite-backed note storage or search remains follow-on work after the file-backed v0.1 release

---

### SPEC-008 · Notes UI

**ID:** SPEC-008  
**Title:** Notes Viewer Window  
**Priority:** P1

#### Description

A native window for browsing, searching, and editing past meeting notes.

Release scope note: v0.1 still opens note files or the notes directory through the user's editor or desktop opener. The native Wayland notes window described here remains follow-on work.

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

All user-configurable options are stored persistently. In v0.1, the tray opens the TOML settings file in the user's editor; a native settings window remains follow-on work.

#### Acceptance Criteria

- [ ] Settings stored as TOML at `~/.config/rustle/config.toml`
- [ ] Settings entry is accessible from the tray menu; in v0.1 it opens the TOML file in the configured editor
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

- [ ] Notification on meeting detected: "📅 Teams meeting detected – recording started"
- [ ] Notification on meeting ended: "✅ Notes ready for the meeting"
- [ ] Notification on transcription/AI error
- [ ] Notifications sent via `notify-rust` (DBus `org.freedesktop.Notifications`)
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
- [ ] Panic handler installed via `std::panic::set_hook`; panics logged before process exits
- [ ] If transcription fails, raw audio file preserved for manual retry
- [ ] If AI summarisation fails, raw transcript preserved and note marked "summarisation pending"
- [ ] Structured logging to file with `tracing`; log rotation after 10MB

---

## 4. Out of Scope (v1)

- Multi-user / team sync
- Google Meet / Zoom detection
- Calendar integration
- Mobile companion app
- End-to-end encryption of notes

---

## 5. Test Strategy

| Spec     | Unit                       | Integration             | Manual                 |
| -------- | -------------------------- | ----------------------- | ---------------------- |
| SPEC-001 | Tray registration mock     | DBus roundtrip test     | Visual tray check      |
| SPEC-002 | File write/delete          | Desktop file parse      | Login smoke test       |
| SPEC-004 | Detection logic unit tests | Hyprland socket mock    | Live Teams call        |
| SPEC-005 | Audio buffer chunking      | cpal device enumeration | Record + playback      |
| SPEC-006 | Segment parsing            | Whisper model load      | Transcribe sample WAV  |
| SPEC-007 | Prompt construction        | API mock                | Review generated notes |
| SPEC-009 | TOML parse/validate        | Config round-trip       | Settings UI smoke      |

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
