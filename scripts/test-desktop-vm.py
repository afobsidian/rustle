#!/usr/bin/env python3
"""Run the installed desktop gate with KVM and a virtual GPU, including on hosted CI."""
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import time

REPO = Path(__file__).resolve().parent.parent
IMAGE_URL = "https://cloud-images.ubuntu.com/minimal/releases/noble/release-20261008/ubuntu-24.04-minimal-cloudimg-amd64.img"
IMAGE_SHA = "2f119f50bf29fe40e73b69ae2a736e8210d8146796f036e0324315b6b3a777df"


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def main(target="all"):
    assert target in ("all", "fedora", "arch", "obsidian-blue")
    work = REPO / "target/desktop-vm"
    work.mkdir(parents=True, exist_ok=True)
    artifacts = REPO / "target/desktop-artifacts"
    models = REPO / "target/desktop-models"
    artifacts.mkdir(parents=True, exist_ok=True)
    models.mkdir(parents=True, exist_ok=True)
    base = work / "base.img"
    if not base.exists():
        run("curl", "--fail", "--location", "--retry", "2", IMAGE_URL, "-o", str(base))
    run("sha256sum", "--check", "--status", input=f"{IMAGE_SHA}  {base}\n", text=True)
    key = work / "id_ed25519"
    if not key.exists():
        run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key))
    (work / "user-data").write_text("#cloud-config\ndisable_root: false\nssh_pwauth: false\nusers:\n"
        "  - name: root\n    ssh_authorized_keys:\n      - " + key.with_suffix(".pub").read_text().strip() + "\n")
    (work / "meta-data").write_text("instance-id: rustle-desktop-gate\nlocal-hostname: rustle-desktop-gate\n")
    run("cloud-localds", str(work / "seed.img"), str(work / "user-data"), str(work / "meta-data"))
    disk = work / "guest.qcow2"
    if not disk.exists():
        run("qemu-img", "create", "-f", "qcow2", "-F", "qcow2", "-b", str(base), str(disk), "70G")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    ssh = ["ssh", "-p", str(port), "-i", str(key), "-o", "StrictHostKeyChecking=no",
           "-o", "UserKnownHostsFile=/dev/null", "-o", "LogLevel=ERROR", "-o", "ConnectTimeout=3", "root@127.0.0.1"]
    memory = min(8192, os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE") // 2**20 - 2048)
    assert memory >= 4096, "the desktop gate needs at least 6 GiB host RAM"
    qemu = ["sudo", "qemu-system-x86_64", "-accel", "kvm", "-cpu", "host",
            "-smp", str(min(4, os.cpu_count() or 1)), "-m", str(memory),
            "-drive", f"file={disk},if=virtio,discard=unmap", "-drive", f"file={work / 'seed.img'},format=raw,if=virtio",
            "-device", "virtio-vga", "-display", "none", "-serial", f"file:{work / 'console.log'}",
            "-netdev", f"user,id=net0,hostfwd=tcp:127.0.0.1:{port}-:22", "-device", "virtio-net-pci,netdev=net0"]
    for tag, path in (("artifacts", artifacts), ("models", models)):
        qemu += ["-virtfs", f"local,path={path},mount_tag={tag},security_model=none,id={tag}"]
    archive = os.environ.get("RUSTLE_IMAGE_ARCHIVE")
    if archive:
        archive = Path(archive).resolve()
        qemu += ["-virtfs", f"local,path={archive.parent},mount_tag=image,security_model=none,readonly=on,id=image"]
    vm = subprocess.Popen(qemu)
    try:
        deadline = time.monotonic() + 180
        while subprocess.run(ssh + ["true"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
            if vm.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError(f"VM did not become ready; see {work / 'console.log'}")
            time.sleep(1)
        run(*ssh, "cloud-init status --wait >/dev/null")
        # Reclaim deleted container layers from previous local test runs.
        run(*ssh, "fstrim -v /")
        # Ubuntu's helper profile also attaches inside Fedora containers and denies
        # their PAM database. Remove it only in this disposable, private test VM.
        run(*ssh, "for profile in /etc/apparmor.d/*unix*chkpwd*; do "
            "[ ! -f \"$profile\" ] || apparmor_parser -R \"$profile\"; done")
        run(*ssh, "export DEBIAN_FRONTEND=noninteractive; apt-get update -qq && apt-get install -y --no-install-recommends podman buildah skopeo curl ca-certificates python3 && "
            "(modprobe virtio_gpu || (apt-get install -y linux-modules-extra-$(uname -r) && modprobe virtio_gpu)) && "
            "test -e /dev/dri/card0 && "
            "mkdir -p /work/rustle /work/obsidian-blue/files/scripts /mnt/artifacts /mnt/models && "
            "(mountpoint -q /mnt/artifacts || mount -t 9p -o trans=virtio,version=9p2000.L artifacts /mnt/artifacts) && "
            "(mountpoint -q /mnt/models || mount -t 9p -o trans=virtio,version=9p2000.L models /mnt/models)")
        paths = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=REPO)
        # git lists deleted tracked paths too; archive only files present in this working tree.
        paths = b"\0".join(path for path in paths.split(b"\0") if path and (REPO / os.fsdecode(path)).exists()) + b"\0"
        source = work / "source.tar.gz"
        run("tar", "-czf", str(source), "--null", "-T", "-", input=paths, cwd=REPO)
        with source.open("rb") as stream:
            run(*ssh, "tar -xzf - -C /work/rustle", stdin=stream)
        env = {"CONTAINER_ENGINE": "podman", "RUSTLE_DESKTOP_ARTIFACTS": "/mnt/artifacts",
               "RUSTLE_DESKTOP_MODELS": "/mnt/models",
               "RUSTLE_VIRTUAL_GPU": "1"}
        if archive:
            assert target == "obsidian-blue", "an image archive is only valid for the installed-image gate"
            run(*ssh, "mkdir -p /mnt/image && mount -t 9p -o trans=virtio,version=9p2000.L,ro image /mnt/image && "
                f"skopeo copy {shlex.quote('oci-archive:/mnt/image/' + archive.name)} containers-storage:localhost/obsidian-blue:rustle-candidate")
            env["RUSTLE_IMAGE_UNDER_TEST"] = "localhost/obsidian-blue:rustle-candidate"
        command = "cd /work/rustle && env " + " ".join(shlex.quote(k + "=" + v) for k, v in env.items())
        result = subprocess.run(ssh + [command + " bash scripts/test-desktop.sh " + shlex.quote(target)])
        run(*ssh, "fstrim -v /")
        run(*ssh, "journalctl --no-pager > /mnt/artifacts/vm-journal.log; chmod -R a+rX /mnt/artifacts")
        (artifacts / "vm.json").write_text(json.dumps({"image_sha256": IMAGE_SHA, "virtual_gpu": "virtio-vga", "target": target, "exit_code": result.returncode}, indent=2))
        if result.returncode:
            raise SystemExit(result.returncode)
    finally:
        try:
            subprocess.run(ssh + ["poweroff"], timeout=10, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except subprocess.TimeoutExpired:
            pass
        try:
            vm.wait(timeout=30)
        except subprocess.TimeoutExpired:
            subprocess.run(["sudo", "kill", "-TERM", str(vm.pid)], check=False)
            vm.wait(timeout=10)


if __name__ == "__main__":
    main(*sys.argv[1:])
