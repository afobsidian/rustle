#!/usr/bin/env bash
set -euo pipefail
[[ "${RUSTLE_VIRTUAL_GPU:-}" == 1 ]] || { echo 'Run python3 scripts/test-desktop-vm.py to isolate graphics and audio.' >&2; exit 2; }
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo/tests/desktop/images.env"
target="${1:-all}"
if [[ "$target" == all ]]; then
  for target in fedora arch obsidian-blue; do bash "$0" "$target"; done
  exit 0
fi
case "$target" in
  fedora) base="$FEDORA_IMAGE" ;;
  arch) base="$ARCH_IMAGE" ;;
  obsidian-blue) base="${RUSTLE_IMAGE_UNDER_TEST:-$OBSIDIAN_IMAGE}" ;;
  *) echo "Unknown desktop target: $target" >&2; exit 2 ;;
esac
engine="${CONTAINER_ENGINE:-podman}"
build_flags=(--layers)
run_flags=(--systemd=always)
if [[ "$engine" == docker ]]; then build_flags=(--builder default); run_flags=(); fi
artifacts="${RUSTLE_DESKTOP_ARTIFACTS:-$repo/target/desktop-artifacts}/$target"
models="${RUSTLE_DESKTOP_MODELS:-$repo/target/desktop-models}"
mkdir -p "$artifacts" "$models"
image="rustle-desktop-test:$target"
containerfile="$repo/tests/desktop/Containerfile"
installed=false
if [[ -n "${RUSTLE_IMAGE_UNDER_TEST:-}" ]]; then
  [[ "$target" == obsidian-blue ]]
  containerfile="$repo/tests/desktop/Installed.Containerfile"
  installed=true
fi
"$engine" build "${build_flags[@]}" -f "$containerfile" --build-arg "BUILD_IMAGE=$BUILD_IMAGE" \
  --build-arg "BASE_IMAGE=$base" -t "$image" "$repo" >"$artifacts/build.log" 2>&1
container="$("$engine" run -d "${run_flags[@]}" --privileged --cgroupns=private --tmpfs /run --tmpfs /tmp --tmpfs /dev/snd \
  -v "$artifacts:/artifacts:rw" -v "$models:/models:rw" "$image")"
trap '"$engine" exec "$container" journalctl --no-pager >"$artifacts/session-journal.log" 2>&1 || true; "$engine" exec "$container" chmod -R a+rX /artifacts 2>/dev/null || true; "$engine" logs "$container" >"$artifacts/container.log" 2>&1; "$engine" rm -f "$container" >/dev/null' EXIT
"$engine" inspect "$image" >"$artifacts/image.json"
# Distro group IDs differ, and a preceding compositor can reset device access.
# These are exclusively the disposable VM's virtual graphics devices.
"$engine" exec "$container" bash -ec 'chmod a+rw /dev/dri/*; for attempt in $(seq 1 150); do test -S /run/dbus/system_bus_socket && break; sleep 0.1; done; chown -R tester:tester /artifacts /models; loginctl enable-linger tester; systemctl start user@1000.service'
"$engine" exec -d -e SEATD_VTBOUND=0 "$container" sh -c 'exec seatd -u tester -g tester >/artifacts/seatd.log 2>&1'
"$engine" exec "$container" bash -ec 'for attempt in $(seq 1 150); do test -S /run/seatd.sock && exit 0; sleep 0.1; done; exit 1'
if [[ "$installed" == true ]]; then
  "$engine" exec "$container" systemctl start system-flatpak-setup.service
fi
"$engine" exec "$container" systemd-run --unit=rustle-desktop-check --wait --pipe --collect --uid=tester \
  -p PAMName=rustle-test -E XDG_RUNTIME_DIR=/run/user/1000 \
  -E DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus \
  -E "RUSTLE_DESKTOP_TARGET=$target" -E "RUSTLE_TEST_INSTALLED_IMAGE=$installed" bash /opt/rustle-test/desktop/session.sh \
  >"$artifacts/test.log" 2>&1
"$engine" cp "$container:/candidate/." "$artifacts/candidate"
echo "PASS: $target installed desktop gate; artifacts: $artifacts"
