"""Production detector against a private session bus and actual Hyprland IPC."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

BUS = "com.github.IsmaelMartinez.teams_for_linux"
PATH = "/com/github/IsmaelMartinez/teams_for_linux"
CONTROL = BUS + ".Control"


def call(member, signature=None, value=None):
    args = ["busctl", "--user", "--timeout=2", "--json=short", "call", BUS, PATH, "org.rustle.Test", member]
    if signature:
        args += [signature, value]
    return subprocess.check_output(args, text=True, timeout=5)


def wait(predicate, description, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.1)
    raise TimeoutError(description)


def api_ready():
    return subprocess.run(["busctl", "--user", "--timeout=1", "call", BUS, PATH, CONTROL, "GetState"],
                          capture_output=True, timeout=3).returncode == 0


def main(binary, artifacts):
    directory = Path(tempfile.mkdtemp(prefix="teams-contract-", dir=artifacts))
    config = directory / "config/rustle/config.toml"
    config.parent.mkdir(parents=True, exist_ok=True)
    config.write_text("[meeting]\nauto_detect=true\nauto_capture=false\n[ai]\nprovider='openai'\n")
    fixture = directory / "speech.txt"
    fixture.write_text("Alice: The release is approved for Friday.")
    env = dict(os.environ, XDG_CONFIG_HOME=str(directory / "config"),
               XDG_DATA_HOME=str(directory / "data"), RUSTLE_TEST_AUDIO_FILE=str(fixture),
               RUSTLE_SELF_TEST_CHILD="1")
    env.pop("HYPRLAND_INSTANCE_SIGNATURE", None)
    server = subprocess.Popen(["/usr/bin/python3", str(Path(__file__).with_name("teams-service.py"))])
    window = subprocess.Popen(["foot", "--app-id=teams-for-linux", "--title=Calendar | Microsoft Teams", "sh", "-c", "sleep 120"])
    process = None
    try:
        wait(api_ready, "simulator API readiness")
        with (directory / "process.log").open("w") as log:
            process = subprocess.Popen([binary], env=env, stdin=subprocess.PIPE, stdout=log, stderr=log,
                                       text=True, start_new_session=True)
            data = directory / "data/rustle"
            drafts = lambda: list((data / "transcripts").glob("*.txt"))
            notes = lambda: list((data / "notes").glob("*.md"))
            wait(lambda: "rustle initialised" in (directory / "process.log").read_text(), "application startup")
            before = json.loads(call("GetCount"))["data"][0]
            wait(lambda: json.loads(call("GetCount"))["data"][0] >= before + 2, "idle polling observations")
            assert not drafts(), "Calendar/idle client triggered recording"
            call("SetInCall", "b", "true")
            wait(lambda: len(drafts()) == 1, "Teams call start")
            first = drafts()[0]
            call("SetInCall", "b", "true")
            window.terminate()
            window.wait(timeout=5)
            call("Malformed", "b", "true")
            before = json.loads(call("GetCount"))["data"][0]
            wait(lambda: json.loads(call("GetCount"))["data"][0] >= before + 2, "malformed state observations")
            assert len(drafts()) == 1 and not notes(), "minimisation/window loss/unknown state ended the call"
            call("Malformed", "b", "false")
            call("SetInCall", "b", "false")
            wait(lambda: len(notes()) == 1, "call finalisation")
            assert first.is_file(), "short call transcript was deleted"
            # A second call survives service restart and returns to idle cleanly.
            call("Quit")
            server.wait(timeout=5)
            server = subprocess.Popen(["/usr/bin/python3", str(Path(__file__).with_name("teams-service.py"))])
            wait(api_ready, "service recovery")
            call("SetInCall", "b", "true")
            wait(lambda: len(drafts()) == 2, "call after Teams restart")
            call("SetInCall", "b", "false")
            wait(lambda: len(notes()) == 2, "second finalisation")
            assert json.loads(call("GetCount"))["data"][0] < 100, "D-Bus monitor feedback loop"
            process.stdin.write("quit\n")
            process.stdin.flush()
            process.wait(timeout=20)
            assert process.returncode == 0
        # Startup during an existing call, stale compositor environment, and SIGTERM.
        call("SetInCall", "b", "true")
        env["HYPRLAND_INSTANCE_SIGNATURE"] = "stale-session"
        with (directory / "restart.log").open("w") as log:
            process = subprocess.Popen([binary], env=env, stdin=subprocess.PIPE,
                                       stdout=log, stderr=log, start_new_session=True)
            wait(lambda: len(drafts()) == 3, "already-active call at application startup")
            process.terminate()
            process.wait(timeout=20)
            assert process.returncode == 0 and len(notes()) == 3, "SIGTERM lost active-call work"
        print(json.dumps({"ok": True, "artifacts": str(directory), "coverage": "simulated Teams protocol + real Hyprland IPC; fallback intentionally exercised"}))
    finally:
        for child in (process, window, server):
            if child is not None and child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)


if __name__ == "__main__":
    main(*sys.argv[1:])
