#!/usr/bin/env bash
set -euo pipefail
archive="$(realpath -- "$1")"
destination="$(realpath -- "$2")"
cd "$destination"
cp "$archive" rustle.tar.gz
tar -xOf rustle.tar.gz usr/bin/rustle >rustle-version-check
chmod +x rustle-version-check
version="$(./rustle-version-check --version | awk '{print $2}')"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
checksum="$(sha256sum rustle.tar.gz | awk '{print $1}')"
cat >PKGBUILD <<EOF
pkgname=rustle
pkgver=$version
pkgrel=1
pkgdesc='Local meeting recording and notes for Hyprland'
arch=('x86_64')
url='https://github.com/afobsidian/rustle'
license=('MIT')
depends=('hyprland' 'pipewire-audio' 'pipewire-pulse' 'wireplumber' 'libpulse' 'systemd' 'dbus' 'flatpak' 'python' 'curl' 'xdg-utils' 'gcc-libs')
source=('rustle.tar.gz')
sha256sums=('$checksum')
package() {
  cp -a usr "\$pkgdir/"
}
EOF
makepkg --nodeps --noconfirm
