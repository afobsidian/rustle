#!/usr/bin/env bash
set -euo pipefail
export RUSTLE_TEST_ARTIFACTS=/artifacts
mkdir -p "$HOME/.local/share/rustle" "$HOME/.config/rustle"
ln -sfnT /models "$HOME/.local/share/rustle/models"
printf '[general]\nstart_on_login = true\nstart_on_login_method = "systemd"\n' >"$HOME/.config/rustle/config.toml"
systemctl --user mask --runtime rustle.service
test -n "${XDG_SESSION_ID:-}"
export XDG_SEAT=seat0
export LIBSEAT_BACKEND=seatd
export AQ_DRM_DEVICES=/dev/dri/card0
trap 'uwsm stop >/dev/null 2>&1 || true' EXIT
if [[ "$RUSTLE_DESKTOP_TARGET" == obsidian-blue ]]; then
  /usr/bin/obsidian-blue-quattro-session >/artifacts/hyprland.log 2>&1 &
else
  uwsm start -g -1 -e -D Hyprland -- start-hyprland >/artifacts/hyprland.log 2>&1 &
fi
for attempt in $(seq 1 300); do
  if hyprctl instances -j | python3 -c 'import json,sys; assert json.load(sys.stdin)' 2>/dev/null; then break; fi
  sleep 0.1
done
export HYPRLAND_INSTANCE_SIGNATURE="$(hyprctl instances -j | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["instance"])')"
export WAYLAND_DISPLAY="$(hyprctl instances -j | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["wl_socket"])')"
hyprland_ready=false
for attempt in $(seq 1 30); do
  if timeout 3 hyprctl clients -j >/artifacts/clients.json \
    && python3 -c 'import json,sys; assert isinstance(json.load(sys.stdin), list)' </artifacts/clients.json 2>/dev/null; then
    hyprland_ready=true
    break
  fi
  sleep 0.1
done
[[ "$hyprland_ready" == true ]] || { echo 'Hyprland IPC did not become ready' >&2; exit 1; }
export DISPLAY="$(systemctl --user show-environment | sed -n 's/^DISPLAY=//p')"
export XDG_SESSION_TYPE=wayland
systemctl --user start pipewire.service pipewire-pulse.service wireplumber.service
for attempt in $(seq 1 150); do pactl info >/dev/null 2>&1 && break; sleep 0.1; done
pactl load-module module-null-sink sink_name=rustle_ci_default rate=16000 channels=1
audio_ready=false
for attempt in $(seq 1 150); do
  if pactl set-default-sink rustle_ci_default >/dev/null 2>&1 \
    && pactl set-default-source rustle_ci_default.monitor >/dev/null 2>&1; then
    audio_ready=true
    break
  fi
  sleep 0.1
done
[[ "$audio_ready" == true ]]
if ! busctl --user status org.kde.StatusNotifierWatcher >/dev/null 2>&1; then
  printf '{"modules-right":["tray"]}\n' >/tmp/rustle-waybar.json
  waybar -c /tmp/rustle-waybar.json >/artifacts/waybar.log 2>&1 &
fi
if ! busctl --user status org.freedesktop.Notifications >/dev/null 2>&1; then
  mako >/artifacts/mako.log 2>&1 &
fi
if [[ "${RUSTLE_TEST_INSTALLED_IMAGE:-false}" == true ]]; then
  teams_branch="$(flatpak list --app --columns=application,version,branch | awk '$1 == "com.github.IsmaelMartinez.teams_for_linux" { split($2, version, "."); if (version[1]>2 || (version[1]==2 && version[2]>=25)) { print $3; exit } }')"
  test -n "$teams_branch"
  teams_ref="com.github.IsmaelMartinez.teams_for_linux//$teams_branch"
else
  flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
  flatpak remote-add --user --if-not-exists flathub-beta https://flathub.org/beta-repo/flathub-beta.flatpakrepo
  teams_ref=com.github.IsmaelMartinez.teams_for_linux//beta
  if ! flatpak install --user --noninteractive flathub-beta "$teams_ref"; then
    teams_ref=com.github.IsmaelMartinez.teams_for_linux//stable
    flatpak install --user --noninteractive flathub "$teams_ref"
  fi
fi
RUSTLE_TEST_TEAMS_SERVICE=/opt/rustle-test/desktop/teams-service.py rustle setup > /artifacts/setup.log 2>&1
rustle self-test > /artifacts/manual-self-test.log 2>&1
flatpak run "$teams_ref" >/artifacts/teams.log 2>&1 &
teams_ready=false
for attempt in $(seq 1 300); do
  if busctl --user --timeout=1 call com.github.IsmaelMartinez.teams_for_linux /com/github/IsmaelMartinez/teams_for_linux \
    com.github.IsmaelMartinez.teams_for_linux.Control GetState >/artifacts/teams-state.txt 2>/dev/null; then
    hyprctl clients -j >/artifacts/teams-windows.json
    if python3 -c 'import json,sys; assert any(c.get("mapped", True) and str(c.get("class", "")).lower() in ("teams-for-linux", "com.github.ismaelmartinez.teams_for_linux") for c in json.load(sys.stdin))' </artifacts/teams-windows.json; then
      teams_ready=true
      break
    fi
  fi
  sleep 0.1
done
[[ "$teams_ready" == true ]]
rustle doctor --json > /artifacts/doctor.json
python3 /opt/rustle-test/verify-install.py /usr
desktop-file-validate /usr/share/applications/rustle.desktop
systemd-analyze --user verify /usr/lib/systemd/user/rustle.service
systemctl --user unmask --runtime rustle.service
systemctl --user enable --now rustle.service
for attempt in $(seq 1 150); do systemctl --user is-active --quiet rustle.service && break; sleep 0.1; done
systemctl --user is-active --quiet rustle.service
rustle_pid="$(systemctl --user show --property=MainPID --value rustle.service)"
printf '%s\n' "$rustle_pid" >/artifacts/rustle-service-pid.txt
if rustle > /artifacts/duplicate.log 2>&1; then echo 'duplicate instance accepted' >&2; exit 1; fi
grep -q 'already running' /artifacts/duplicate.log
for attempt in $(seq 1 150); do
  if busctl --user get-property org.kde.StatusNotifierWatcher /StatusNotifierWatcher org.kde.StatusNotifierWatcher RegisteredStatusNotifierItems >/artifacts/tray.txt 2>/dev/null \
    && grep -Fq "org.kde.StatusNotifierItem-$rustle_pid-" /artifacts/tray.txt; then break; fi
  sleep 0.1
done
grep -Fq "org.kde.StatusNotifierItem-$rustle_pid-" /artifacts/tray.txt
systemctl --user restart rustle.service
systemctl --user is-active --quiet rustle.service
systemctl --user stop rustle.service
flatpak kill com.github.IsmaelMartinez.teams_for_linux
dbus-run-session -- /usr/bin/python3 /opt/rustle-test/desktop/contracts.py /usr/bin/rustle /artifacts
flatpak list --user --columns=application,version,branch > /artifacts/flatpaks.txt
hyprctl version > /artifacts/hyprland-version.txt
pw-dump > /artifacts/final-graph.json
systemctl --user show-environment | sed -n '/^HYPRLAND_INSTANCE_SIGNATURE=/p;/^WAYLAND_DISPLAY=/p' > /artifacts/session-environment.txt
journalctl --user -u rustle.service --no-pager > /artifacts/rustle-journal.log
echo 'PASS: installed desktop, real Flatpak API, real audio/models, simulated call lifecycle, session service'
