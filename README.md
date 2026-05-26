# Rustle

Rust-native desktop meeting notes app for Fedora Linux on Hyprland.

## Specification

See [docs/specification.md](docs/specification.md) for the product and technical specification.

## Run Rustle

Rustle starts as a StatusNotifierItem system tray app on Hyprland with an SNI host such as Waybar. It keeps terminal controls as a fallback when a tray host or D-Bus is unavailable, warns when launched outside Hyprland, starts audio capture for meetings when `meeting.auto_capture = true`, transcribes completed WAV chunks through the configured local Whisper model, summarizes the transcript through the configured AI provider, and writes Markdown notes.

```sh
make run
```

Tray actions include opening notes, opening the latest transcript draft, starting or stopping a manual meeting, opening the settings TOML file, and quitting Rustle. Left-clicking the tray icon opens notes.

Available fallback terminal commands while Rustle is running:

```text
start [meeting name]
stop
open
transcript
settings
quit
help
```

The audio recorder uses the first available Linux recording tool in this order: `pw-record` (PipeWire), `parecord` (PulseAudio), then `arecord` (ALSA). On Fedora/PipeWire systems this should work with the OS-provided PipeWire tools; no extra Rustle-specific service is required. Recordings are saved as 16 kHz mono WAV chunks under `~/.local/share/rustle/recordings/`.

To test transcription with a known WAV instead of the live mic, run Rustle with `RUSTLE_TEST_AUDIO_FILE=/path/to/sample.wav`. Live audio capture is skipped for that run, and the WAV is transcribed when you enter `stop` for the manual meeting.

Local transcription uses `whisper-rs` and reads the model configured by `transcription.model_path`. When the default model path is used, Rustle downloads the whisper.cpp `ggml-base.en.bin` model on first transcription if it is missing:

```toml
[transcription]
method = "local"
model_path = "~/.local/share/rustle/models/ggml-base.en.bin"
```

For a custom `transcription.model_path`, download or place a compatible whisper.cpp `ggml` model at that path yourself.

The default AI provider is the local `llama_cpp` backend using a quantized GGUF model. The first run may download model files, and failures still fall back to locally generated Markdown warning notes.

To point Rustle at a different local llama.cpp model, set this in `~/.config/rustle/config.toml`:

```toml
[ai]
provider = "llama_cpp"
model = "Qwen/Qwen2.5-3B-Instruct-GGUF"
hf_repo = "Qwen/Qwen2.5-3B-Instruct-GGUF"
hf_model_file = "qwen2.5-3b-instruct-q4_k_m.gguf"
```

Ollama remains available as an optional provider by setting `provider = "ollama"`. If the configured AI provider is unavailable, Rustle still saves fallback notes with the failure reason. Transcript drafts are created under `~/.local/share/rustle/transcripts/` so you can monitor or manually edit transcript text, and Markdown notes are saved under `~/.local/share/rustle/notes/` without embedding the transcript. Settings are stored at `~/.config/rustle/config.toml` and can be opened from the tray menu.

Run `make bench-ai` to compare several suitable GGUF models through the local llama.cpp backend on this machine.

Hyprland-based Teams detection and a native notes window are still future work; the current app wires tray controls, application lifecycle, audio-backed transcription, and note-generation pipeline so it can be exercised end to end.

## DevOps

Use `make ci` to run the local validation gate. See [docs/devops.md](docs/devops.md) for test, build, release, and pipeline procedures.
