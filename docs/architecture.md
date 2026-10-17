# Runtime architecture

Rustle is a tray-first, file-backed application for x86-64 Hyprland on obsidian-blue,
Fedora, and Arch. Feature crates communicate through rustle-core's Tokio event bus;
none depends on another feature crate to control its lifecycle.

## Startup and readiness

The binary installs diagnostics, acquires an advisory instance lock, loads settings,
and reconciles user startup preferences. `--session` respects explicit disablement;
its first invocation opts a new installation into graphical-session startup.

First-run setup merges Teams Flatpak D-Bus enablement with an existing configuration,
backs it up, and provisions checksum-verified pinned local models. A child installed
process then exercises virtual capture, transcription, and note persistence. The
self-test uses its own session bus, PipeWire server, hardware-disabled WirePlumber
policy, and storage. Only successful inference and cleanup produce a readiness
fingerprint. JSON doctor checks report actual required capabilities separately.

Every subsystem receiver, including the shutdown coordinator, is subscribed before
any subsystem can emit events. Detection and terminal controls can therefore start
immediately without losing a meeting-start event.

## Ownership

| Component | Owns |
| --- | --- |
| root binary | Setup, diagnostics, singleton lock, session startup, shutdown coordination. |
| rustle-core | Settings, paths, domain types, event definitions and event bus. |
| rustle-detection | Teams D-Bus observations, Hyprland names/browser compatibility, reconnection and call identity. |
| rustle-audio | Recorder and temporary PipeWire graph processes, rotation, device changes, final WAV flush. |
| rustle-transcription | Unique drafts, queued Whisper jobs, chunk ordering, transcript finalisation. |
| rustle-ai | Local notes generation and explicit fallback status. |
| rustle-storage | Atomic unique note publication, managed document listing/deletion. |
| rustle-tray / rustle-ui | Tray and terminal controls; desktop/editor document opening. |

## Detection

Teams for Linux >=2.25.0 is observed through GetState and InCallChanged on the user
session bus. Monitoring signals triggers a snapshot; periodic snapshots recover
missed signals and restarts. Only `inCall` starts recording. Window names provide
labels; hiding or closing a window, incoming ringing, and microphone mute do not
end an active call. An invalid observation while the D-Bus owner exists retains
current state. The upstream service reflects the selected Teams profile only.

Native and Flatpak Teams classes are excluded from title-only detection. Existing
browser-title detection remains compatible. Hyprland socket2 events supplement
polling and reconnect with bounded backoff. Queries have timeouts. A manual meeting
suppresses automatic duplicate capture, and replacement recordings flush first.

## Recording-to-note lifecycle

1. MeetingStarted establishes a stable ID and a unique transcript draft.
2. Audio owns capture completion. It emits RecordingStopped on every path,
   including disabled/fixture capture and startup failure. Transcription waits
   for that acknowledgement even when MeetingEnded arrives before audio readiness.
3. Mixed recording creates a temporary sink/source fed by the microphone and
   selected sink monitor, then records 16 kHz mono WAV. Default-device changes
   reconnect feeds; rotation validates and publishes each completed chunk.
4. RecordingChunkReady queues real Whisper work without blocking event handling.
   Finalisation waits for all chunks, MeetingEnded, and RecordingStopped.
5. TranscriptionReady starts notes processing. TranscriptImported names an existing
   transcript without emitting MeetingStarted or accidentally capturing audio.
6. SummarisationReady reaches storage. Notes are written to a private temporary
   file, synced, and atomically renamed to a collision-resistant final name.
7. NoteSaved reports the path. WorkflowFailed describes degraded/error stages;
   fallback notes remain explicit failures, even if a recovery file is saved.
   MeetingFinished acknowledges completed processing, including empty/error cases.

Short meetings are retained. Recordings and drafts are not deleted to compensate
for uncertain detection. Recorder shutdown is bounded and reaps processes; startup
validates WAV data rather than treating an arbitrary file size as audio.

## Shutdown and settings

QuitRequested, Ctrl-C, and SIGTERM stop accepting work and end pending meetings.
Processing subscribers remain alive until MeetingFinished has drained the pending
set. ShutdownComplete then releases background loops. A bounded drain timeout
exits with an error and retains source material.

SettingsChanged carries boxed Settings to keep event sizes bounded; the serialized
settings shape remains compatible. audio.output_device defaults to `default` and
fresh configurations enable capture_loopback. Existing explicit preferences remain
unchanged. Login services attach to graphical-session.target rather than the user
manager's earlier default.target, and normal non-UWSM Hyprland startup remains
available through the binary/XDG integration.

## Validation boundary

The full release gate installs the candidate on actual distro userlands and runs
nested Hyprland, UWSM, real PipeWire, the Teams Flatpak API, and real local models.
Protocol simulation is isolated and identified separately. Authenticated Microsoft
calls and physical hardware are not represented as tested by that gate. See
[DevOps](devops.md) for commands and required evidence.
