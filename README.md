# Rustle

Rust-native desktop meeting notes app for Fedora Linux on Hyprland/Wayland.

## Specification

See [docs/specification.md](docs/specification.md) for the product and technical specification.

## Run the manual notes MVP

The current working slice is a terminal-controlled manual notes flow. It keeps the process alive, creates a transcript draft, summarizes it through the configured AI provider, and writes Markdown notes.

```sh
make run
```

Available commands while Rustle is running:

```text
start [meeting name]
stop
open
quit
help
```

The default AI provider is the local `llama_cpp` backend using a quantized GGUF model. The first run may download model files, and failures still fall back to locally generated Markdown notes from the transcript.

To point Rustle at a different local llama.cpp model, set this in `~/.config/rustle/config.toml`:

```toml
[ai]
provider = "llama_cpp"
model = "Qwen/Qwen2.5-0.5B-Instruct-GGUF"
hf_repo = "Qwen/Qwen2.5-0.5B-Instruct-GGUF"
hf_model_file = "qwen2.5-0.5b-instruct-q4_k_m.gguf"
```

Ollama remains available as an optional provider by setting `provider = "ollama"`. If the configured AI provider is unavailable, Rustle still saves fallback notes with the raw transcript. Transcript drafts are created under `~/.local/share/rustle/transcripts/` with sample meeting text that you can edit or replace, and Markdown notes are saved under `~/.local/share/rustle/notes/`.

Run `make bench-ai` to compare several suitable GGUF models through the local llama.cpp backend on this machine.

The tray, Teams detection, audio capture, and native notes window are still future work; this MVP wires the application lifecycle and note-generation pipeline so it can be exercised end to end.

## DevOps

Use `make ci` to run the local validation gate. See [docs/DEVOPS.md](docs/DEVOPS.md) for test, build, release, and pipeline procedures.
