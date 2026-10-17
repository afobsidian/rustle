"""Check the installed layout and run the binary outside the checkout."""
import configparser
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def verify(prefix):
    prefix = Path(prefix).resolve()
    required = ["bin/rustle", "share/rustle/icons/rustle.svg",
                "share/rustle/icons/rustle-recording.svg",
                "share/icons/hicolor/scalable/apps/rustle.svg",
                "share/applications/rustle.desktop", "lib/systemd/user/rustle.service"]
    for path in required:
        assert (prefix / path).is_file(), f"missing installed file: {path}"
    binary = prefix / "bin/rustle"
    assert os.access(binary, os.X_OK), "binary is not executable"
    desktop = configparser.ConfigParser(interpolation=None)
    desktop.read(prefix / "share/applications/rustle.desktop")
    assert desktop["Desktop Entry"]["Exec"] == "rustle"
    assert desktop["Desktop Entry"]["Icon"] == "rustle"
    service = (prefix / "lib/systemd/user/rustle.service").read_text()
    assert "--session" in service and "WantedBy=graphical-session.target" in service
    with tempfile.TemporaryDirectory(prefix="rustle-install-check-") as directory:
        env = dict(os.environ, XDG_CONFIG_HOME=f"{directory}/config", XDG_DATA_HOME=f"{directory}/data")
        result = subprocess.run([str(binary), "--version"], env=env, cwd=directory,
                                capture_output=True, text=True, check=True, timeout=15)
        assert result.stdout.startswith("rustle "), result.stdout
        linked = subprocess.run(["ldd", str(binary)], capture_output=True, text=True, timeout=15)
        assert linked.returncode == 0 and "not found" not in linked.stdout + linked.stderr, linked.stdout + linked.stderr
        # An explicit user disablement must stop graphical-session startup before setup/downloads.
        config = Path(directory) / "config/rustle/config.toml"
        config.parent.mkdir(parents=True, exist_ok=True)
        contents = "[general]\nstart_on_login = false\n"
        config.write_text(contents)
        subprocess.run([str(binary), "--session"], env=env, cwd=directory,
                       capture_output=True, check=True, timeout=15)
        assert config.read_text() == contents, "session startup overwrote user preferences"
    print(f"PASS: installed layout, shared libraries, standalone launch, user disablement ({prefix})")


if __name__ == "__main__":
    verify(sys.argv[1])
