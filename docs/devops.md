# Development and automated release gates

Rustle supports x86-64 Hyprland with PipeWire/WirePlumber on obsidian-blue, Fedora,
and Arch. A passing unit suite alone does not qualify a release.

## Fast gate

```sh
make ci
```

This checks formatting, runs Clippy with warnings denied, workspace tests, a
workspace build, and Python harness contract tests. CI runs Rust 1.88.0 and stable.
`Cargo.lock` is committed. Dependency resolution honours the MSRV; the llama.cpp
binding is pinned to the API exercised by the code.

## Installed desktop gate

Requires KVM, QEMU, cloud-image-utils, SSH, Python 3 and sudo. Podman runs
privileged disposable containers inside the VM; no host GPU is used. The companion
obsidian-blue checkout must contain the new installer, including for candidate
validation before a release is published:

```sh
make test-desktop
make test-desktop DESKTOP_TARGET=fedora
make test-desktop DESKTOP_TARGET=arch
make test-desktop DESKTOP_TARGET=obsidian-blue
```

`tests/desktop/images.env` pins base image digests. Fedora obtains Hyprland and
UWSM from the signed COPR used by obsidian-blue. All targets use the same Fedora 43
candidate build (with a fixed AVX2 CPU baseline) and install its actual package layout. The obsidian-blue target
also runs the companion image installer against that candidate and starts its
actual Quattro session wrapper.

The harness boots a checksum-pinned Ubuntu VM using KVM and a virtio virtual GPU,
then runs the distribution containers through Podman. Each container starts an
isolated PAM/systemd user session, Hyprland with software rendering on the virtual
GPU, PipeWire, WirePlumber, a tray host, and a notification service. It installs a Teams Flatpak meeting the minimum version
and verifies the real D-Bus endpoint without signing into Microsoft.
Podman supplies container DNS; physical-device discovery, the local DNS stub, and
the login greeter are disabled in the test containers. The gate starts its own
PAM session through the installed compositor launcher.
Candidate-package tests install Teams explicitly and disable unrelated background
Flatpak provisioning; the actual-image gate runs the image's provisioning unit.

Required scenarios:

| Scenario | Acceptance |
| --- | --- |
| Package | Installed assets, executable dependencies, launcher and user unit validate outside the checkout. |
| Trigger, audio and models | A simulated Teams call starts the installed process recording automatically; private 48 kHz microphone and stereo output speech survive production capture, real Whisper, and real Qwen inference. |
| Output changes and rotation | Switching the private default output reconnects capture; multiple valid chunks produce one final note. |
| Transient audio queries | One deliberately stalled real device query leaves capture running; persistent query failures are bounded by a crate-local regression test. |
| Notes | The explicit Friday approval appears under decisions; Morgan's checklist and Thursday deadline appear under action items. Fallback warnings fail this scenario. |
| Short call and quit | Separate automatic-call and manual-capture runs retain drafts/recordings; quitting during capture waits for persisted notes and removes the mixing graph. |
| Real Teams API | The installed Flatpak exports GetState after setup; doctor reports required capabilities. |
| Protocol contract | A private D-Bus simulator tests idle, duplicate signals, window loss, malformed observations, ending, and Teams service restart. This is labelled simulated coverage. |
| Session integration | The installed user service starts and restarts under UWSM, registers with the tray host, and rejects duplicate processes. |
| User disablement | `--session` exits without setup or configuration changes when startup is explicitly disabled. |

No scenario requires manual post-install testing. Missing dependencies, unsupported
Teams versions, unavailable beta/stable builds, or failed environment startup fail
the gate. No required environment check is silently skipped.

Evidence is retained in `target/desktop-artifacts/<target>/`: build and session
logs, image identity, actual Teams version/state, doctor JSON, synthetic WAVs,
transcripts, notes, journals, and tested packages. VM diagnostics are stored beside that evidence and in `target/desktop-vm/console.log`.
The host needs KVM, QEMU, cloud-image-utils, SSH, and Python; standard GitHub-hosted
Linux runners install these automatically. No host render device is required. Only synthetic data goes
into CI artifacts. Host GPU drivers, physical microphones, and authenticated
Microsoft service behaviour are outside headless coverage.

## First-run readiness and targeted checks

```sh
rustle setup
rustle doctor --json
RUSTLE_TEST_ARTIFACTS=/path/to/evidence rustle self-test
```

Setup enables Teams D-Bus without replacing other keys, validates pinned model
hashes, and runs the installed-process self-test. It records readiness only after
that test succeeds. Configuration/model metadata and application version changes
invalidate the record. Custom local models must pass inference too. Existing
files survive processing failures.

The self-test creates a private session bus and PipeWire server with hardware
monitors disabled; it does not alter host audio routing or use a physical mic.
`RUSTLE_TEST_AUDIO_FILE` text/WAV fixtures remain useful for deterministic workflow
tests but do not satisfy real recording/inference acceptance.

To validate the distribution layout separately:

```sh
make release-build
make stage DESTDIR="$PWD/target/install-check" PREFIX=/usr
make verify-install PREFIX="$PWD/target/install-check/usr"
```

## Release

1. Set the Cargo package version before creating the matching `vX.Y.Z` tag.
2. Run fast and installed desktop gates. The release workflow repeats both.
3. Publish only packages exported by those successful desktop runs. The workflow
   checks the tag/version and that each target tested the same binary.
4. Release artifacts include the full-layout tarball, Fedora RPM, Arch package,
   and SHA-256 sidecars. Do not publish an Ubuntu-built binary as an untested
   Fedora package.
5. Update obsidian-blue's pinned Rustle version and build its image. Its installer
   verifies the release checksum and records it in the immutable image. A local
   candidate can be validated before publication via `RUSTLE_CANDIDATE_DIR`.

Both repositories must contain their integration changes before cross-repository
CI can pass. Image builds using a not-yet-published version fail until the tested
Rustle release is published; they never fall back to an older release.

## Regression traceability

| Contract | Automated proof |
| --- | --- |
| SPEC-009 / SPEC-012 | Settings defaults, compatibility, permissions, and event bus tests. |
| SPEC-021 / SPEC-023 | Deterministic transcript/fallback workflow and empty-transcript guard. |
| SPEC-024 | End-before-recorder-readiness holds finalisation; final-chunk failure is reported, never converted into fake speech. |
| SPEC-028 | Short Hyprland meeting content remains available. |
| Teams API / SPEC-022 | Crate-local title/state tests plus `tests/desktop/contracts.py`. |
| Installation | `scripts/verify-install.py`, Arch packaging, RPM installation, and the companion image installer tests. |
| SPEC-029 | Transcript import cannot start a recording. |
| Full processing | `scripts/self-test.py`, shared by first setup and desktop gates. |

These describe required checks, not a declaration that an unexecuted environment
has passed. Consult the actual run artifacts for results.
