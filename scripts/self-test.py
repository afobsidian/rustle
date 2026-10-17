"""Installed-process acceptance check. Only virtual audio devices are recorded."""
import json
import os
import shlex
import shutil
import signal
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import wave


def command(*args):
    return subprocess.check_output(args, text=True, timeout=15).strip()


def wait_for(predicate, process, description, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        if process is not None and process.poll() is not None:
            raise RuntimeError(f"Rustle exited before {description}: {process.returncode}")
        time.sleep(0.1)
    raise TimeoutError(description)


def exercise(binary, work, microphone, output):
    modules = []
    process = None
    teams = None
    try:
        for sink in (microphone, output):
            modules.append(command("pactl", "load-module", "module-null-sink", f"sink_name={sink}", "rate=48000",
                                   "channels=1" if sink == microphone else "channels=2"))
        # Pulse can accept connections before WirePlumber publishes default metadata.
        wait_for(lambda: subprocess.run(["pactl", "set-default-sink", output],
                                       capture_output=True, timeout=5).returncode == 0,
                 None, "private audio policy readiness", 15)
        if service := os.environ.get("RUSTLE_TEST_TEAMS_SERVICE"):
            teams = subprocess.Popen(["/usr/bin/python3", service])
            bus = "com.github.IsmaelMartinez.teams_for_linux"
            endpoint = ["busctl", "--user", "--timeout=2", "call", bus,
                        "/com/github/IsmaelMartinez/teams_for_linux"]
            wait_for(lambda: subprocess.run(endpoint + [bus + ".Control", "GetState"],
                                           capture_output=True, timeout=5).returncode == 0,
                     None, "private Teams protocol service", 15)
            command(*endpoint, "org.rustle.Test", "SetInCall", "b", "true")
        env = dict(os.environ, XDG_CONFIG_HOME=str(work / "config"),
                   XDG_DATA_HOME=str(work / "data"), RUSTLE_SELF_TEST_CHILD="1", RUST_LOG="info")
        env.pop("RUSTLE_TEST_AUDIO_FILE", None)
        # Delay one real device query after capture starts; audio must keep flowing.
        commands = work / "bin"
        commands.mkdir()
        pending = work / "slow-query"
        delayed = work / "query-delayed"
        pactl = shutil.which("pactl")
        assert pactl, "pactl is required"
        shim = commands / "pactl"
        shim.write_text("#!/bin/sh\nif [ \"$1\" = --format=json ] && mv " +
                        shlex.quote(str(pending)) + " " + shlex.quote(str(delayed)) +
                        " 2>/dev/null; then sleep 4; fi\nexec " + shlex.quote(pactl) + ' "$@"\n')
        shim.chmod(0o755)
        env["PATH"] = str(commands) + os.pathsep + env.get("PATH", "")
        with (work / "process.log").open("w") as log:
            process = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=log, stderr=log,
                                       text=True, env=env, start_new_session=True)

            def send(text):
                process.stdin.write(text + "\n")
                process.stdin.flush()

            if teams is None:
                send("start Rustle Installed Acceptance")
            wait_for(lambda: "audio recording started" in (work / "process.log").read_text(),
                     process, "recording readiness", 40)
            pending.touch()
            for _ in range(4):
                subprocess.run(["pw-play", f"--target={microphone}", str(work / "microphone.wav")], check=True, timeout=30)
            switched = output + "_switched"
            modules.append(command("pactl", "load-module", "module-null-sink", f"sink_name={switched}", "rate=48000", "channels=2"))
            command("pactl", "set-default-sink", switched)

            def rebound():
                assert "workflow failed" not in (work / "process.log").read_text().lower(), "capture failed; see process.log"
                return any(node.get("info", {}).get("props", {}).get("target.object") == switched
                           and node.get("info", {}).get("state") == "running"
                           for node in json.loads(command("pw-dump")))

            wait_for(rebound, process, "default output device reconnection", 15)
            wait_for(lambda: len(list((work / "data/rustle/recordings").glob("*.wav"))) >= 2,
                     process, "recording chunk rotation", 20)
            subprocess.run(["pw-play", f"--target={switched}", str(work / "output.wav")], check=True, timeout=30)
            if teams is not None:
                command(*endpoint, "org.rustle.Test", "SetInCall", "b", "false")

                def automatic_notes_ready():
                    assert "workflow failed" not in (work / "process.log").read_text().lower(), "automatic workflow failed"
                    return any((work / "data/rustle/notes").glob("*.md"))

                wait_for(automatic_notes_ready, process, "automatic call-to-note completion", 600)
            # Manual mode quits during capture; every mode must exit with work drained.
            send("quit")
            process.wait(timeout=600)
            assert process.returncode == 0, f"unclean shutdown: {process.returncode}"

        data = work / "data/rustle"
        recordings = list((data / "recordings").glob("*.wav"))
        assert len(recordings) >= 2, "chunk rotation was not exercised"
        for recording in recordings:
            with wave.open(str(recording)) as audio:
                assert (audio.getnchannels(), audio.getframerate(), audio.getsampwidth()) == (1, 16000, 2)
                assert audio.getnframes() > 16000, "empty/truncated recording"
        transcripts = list((data / "transcripts").glob("*.txt"))
        notes = list((data / "notes").glob("*.md"))
        assert len(transcripts) == len(notes) == 1, "expected exactly one retained transcript and note"
        transcript = transcripts[0].read_text().lower()
        note = notes[0].read_text().lower()
        for expected in ("friday", "checklist", "thursday"):
            assert expected in transcript, f"missing microphone/output speech: {expected}"
            assert expected in note, f"notes lost the decision or action: {expected}"
        for heading in ("summary", "key decisions", "action items", "attendees"):
            assert heading in note, f"missing notes section: {heading}"
        decisions = note.split("## key decisions", 1)[1].split("## ", 1)[0]
        actions = note.split("## action items", 1)[1].split("## ", 1)[0]
        assert "approv" in decisions and "friday" in decisions, "explicit approval missing from decisions"
        assert all(word in actions for word in ("morgan", "checklist", "thursday")), "action owner/deadline lost"
        assert "## warning" not in note, "fallback notes are not successful inference"
        log = (work / "process.log").read_text().lower()
        assert "workflow failed" not in log, "a workflow stage failed"
        assert delayed.exists() and "keeping current connections and retrying" in log, "transient device query recovery was not exercised"
        assert "shutdown complete; pending meetings drained" in log, "shutdown did not drain"
        graph = json.loads(command("pw-dump"))
        assert not any("rustle_mix_" in str(node) or "rustle_capture_" in str(node) for node in graph), "temporary recording graph leaked"
        print(json.dumps({"ok": True, "artifacts": str(work), "checks": [
            "installed_process", "microphone_and_output", "valid_wav", "real_whisper",
            "real_notes_model", "semantic_notes", "output_device_change", "chunk_rotation",
            "short_meeting_retained", "quit_drained", "graph_cleanup", "transient_audio_query_recovery"] +
            (["teams_trigger_to_real_notes"] if teams is not None else [])}))
    finally:
        if process is not None and process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
        if teams is not None:
            teams.terminate()
            teams.wait(timeout=5)
        for module in reversed(modules):
            subprocess.run(["pactl", "unload-module", module], capture_output=True, timeout=15)



def main(binary, work, microphone, output):
    work = Path(work)
    # AF_UNIX paths are limited to 108 bytes even when artifact paths are long.
    runtime_directory = tempfile.TemporaryDirectory(prefix="rustle-pw-", dir="/tmp")
    runtime = Path(runtime_directory.name)
    os.environ.update(XDG_RUNTIME_DIR=str(runtime), XDG_CONFIG_HOME=str(work / "config"),
                      XDG_DATA_HOME=str(work / "data"), PULSE_SERVER=f"unix:{runtime}/pulse/native",
                      PIPEWIRE_REMOTE="pipewire-0")
    os.environ.pop("HYPRLAND_INSTANCE_SIGNATURE", None)
    # A private server with hardware monitors disabled: setup must never move an
    # ongoing call, record a physical microphone, or change the user's defaults.
    policy = work / "config/wireplumber/wireplumber.conf.d"
    policy.mkdir(parents=True)
    (policy / "rustle-test.conf").write_text("""wireplumber.profiles = {
      main = {
        monitor.alsa = disabled
        monitor.alsa-midi = disabled
        monitor.bluez = disabled
        monitor.libcamera = disabled
        monitor.v4l2 = disabled
      }
    }
    """)
    services = []
    try:
        for program in ("pipewire", "pipewire-pulse", "wireplumber"):
            with (work / f"{program}.log").open("w") as log:
                services.append(subprocess.Popen([program], stdout=log, stderr=log, start_new_session=True))
            if program == "pipewire":
                wait_for(lambda: subprocess.run(["pw-cli", "info", "0"],
                                               capture_output=True, timeout=5).returncode == 0,
                         services[-1], "private PipeWire core readiness", 15)
        deadline = time.monotonic() + 15
        while subprocess.run(["pactl", "info"], capture_output=True, timeout=5).returncode:
            if time.monotonic() > deadline:
                raise TimeoutError("private PipeWire server startup")
            time.sleep(0.1)
        graph = json.loads(command("pw-dump"))
        assert not any(node.get("info", {}).get("props", {}).get("device.api") in ("alsa", "bluez5") for node in graph), "self-test must not access physical devices"
        exercise(binary, work, microphone, output)
    finally:
        for service in reversed(services):
            service.terminate()
            try:
                service.wait(timeout=10)
            except subprocess.TimeoutExpired:
                service.kill()
                service.wait(timeout=5)
        runtime_directory.cleanup()


if __name__ == "__main__":
    main(*sys.argv[1:])
