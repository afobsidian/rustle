# Rustle

Local meeting recording, transcription, and Markdown notes for x86-64 Hyprland on
obsidian-blue, Fedora, and Arch.

## Requirements and first setup

The x86-64 release requires an AVX2 CPU with FMA, F16C, BMI2 and SSE4.2
(Haswell/Zen or newer). Setup and doctor check this before local inference.

Automatic desktop Teams detection requires **Teams for Linux 2.25.0 or newer** with
its session D-Bus API enabled. Rustle reads call state; Calendar, Chat, incoming
ringing, window focus, and microphone mute do not constitute a joined call.
The upstream API reports the selected Teams profile, not every account at once.

If stable Flatpak is older, install and launch beta explicitly:

```sh
flatpak remote-add --if-not-exists flathub-beta https://flathub.org/beta-repo/flathub-beta.flatpakrepo
flatpak install flathub-beta com.github.IsmaelMartinez.teams_for_linux//beta
flatpak run com.github.IsmaelMartinez.teams_for_linux//beta
```

See the [upstream installation guide](https://ismaelmartinez.github.io/teams-for-linux/installation/)
and [D-Bus API](https://ismaelmartinez.github.io/teams-for-linux/development/dbus-control/).

Install PipeWire, its PulseAudio compatibility server, WirePlumber, `pactl`, the
PipeWire command-line tools, systemd/D-Bus, Python 3, curl, and xdg-utils. Fedora's
recorder package is `pipewire-utils`; Arch's is `pipewire-audio`. The RPM declares
runtime dependencies, and obsidian-blue installs them as required packages.

```sh
rustle setup
rustle doctor --json
rustle
```

Setup preserves Teams configuration, backs it up before enabling D-Bus, downloads
and SHA-256 verifies pinned Whisper and Qwen models (about 2.3 GB), then runs an
automatic capture-to-note test. Restart Teams after changing its D-Bus setting.
The first normal launch also performs setup when no matching readiness record
exists. Setup errors appear in diagnostics and desktop notifications.

The self-test runs private PipeWire and D-Bus services with hardware monitors
disabled. It records synthetic speech from virtual devices, exercises default
output switching and recording rotation, and validates real transcription,
notes, file retention, shutdown, and cleanup. It does not change your desktop's
audio defaults or record a physical microphone.

## Recording and notes

Fresh configurations capture the microphone **and selected audio output**, so
remote participants are included with headphones. Other sounds on that output
are included too. Explicit existing preferences are preserved:

```toml
[audio]
input_device = "default"
output_device = "default"
capture_loopback = true
max_recording_size_mb = 2048
chunk_duration_minutes = 10
```

Use a PipeWire node name for a fixed device. `default` follows default-device
changes during capture. Mixed capture uses temporary PipeWire nodes; it never
feeds recorded audio back to physical speakers. Microphone-only recording also
retains the existing PulseAudio/ALSA recorder fallbacks.

Recordings are 16 kHz mono WAV chunks. Meeting end waits for final recording and
transcription work. Quit and SIGTERM drain pending processing before exit; an
explicit timeout reports failure and retains source files. Short meetings are
kept. File names include unique IDs, and notes are published atomically.

Data lives under `$XDG_DATA_HOME/rustle` (normally `~/.local/share/rustle`):
`recordings/`, `transcripts/`, `notes/`, and `logs/rustle.log`. Configuration is
`$XDG_CONFIG_HOME/rustle/config.toml` (normally `~/.config/rustle/config.toml`).
Local Whisper and llama.cpp are the default providers; Ollama is optional.
Unsupported hosted providers produce observable fallback notes and fail a
successful-processing test. Existing browser-title detection remains available;
Teams desktop windows use D-Bus instead of title heuristics.

The tray opens drafts, notes, settings, and managed document folders through the
desktop opener. Terminal controls remain available:

```text
start [meeting name]
stop
open
notes
transcript
settings
quit
help
```

Re-summarising an existing transcript does not start a recording. For manual-only
use without Teams, set `meeting.auto_detect = false` before setup.

## Installation and automatic verification

```sh
make install                         # development binary and launcher in ~/.local
make ci                              # formatting, Clippy, workspace tests, build
make test-desktop                    # required installed desktop matrix
rustle self-test                     # repeat private capture-to-note verification
```

The desktop matrix installs the candidate RPM on Fedora and obsidian-blue and a
native package on Arch, then tests actual Hyprland, UWSM, PipeWire, tray,
notifications, and the required Teams Flatpak API. Synthetic protocol tests cover
call lifecycle; authenticated Microsoft calls are outside this gate. Release
publishing depends on every gate passing; missing required services fail the job.

obsidian-blue installs the pinned release, verifies its checksum, and starts
Rustle with the graphical session. Install a Teams Flatpak meeting the minimum
version separately. Existing `start_on_login = false` is respected. Other installations can
enable the supplied `rustle.service`; a bare Hyprland session can launch `rustle`
from its own startup configuration without depending on UWSM.

See [DevOps and release procedures](docs/devops.md),
[architecture](docs/architecture.md), and [specification](docs/specification.md).
