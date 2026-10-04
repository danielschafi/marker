#!/usr/bin/env bash
# Build Marker (release) and install it as the user-local default PDF app.
#
# Usage:
#   ./packaging/install-local.sh              # cargo build --profile dist, then install
#   ./packaging/install-local.sh /path/to/bin # install a specific binary (skip build)
#
# Installs into ${PREFIX:-$HOME/.local}: binary, .desktop entry, icon, and
# sets application/pdf → marker.desktop via xdg-mime.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
prefix="${PREFIX:-$HOME/.local}"
bin_dir="$prefix/bin"
app_dir="$prefix/share/applications"
icon_dir="$prefix/share/icons/hicolor/scalable/apps"

binary="${1:-}"
if [[ -z "$binary" ]]; then
  echo "Building dist binary..."
  cargo build --profile dist --manifest-path "$root/Cargo.toml"
  binary="$root/target/dist/marker"
elif [[ ! -x "$binary" ]]; then
  echo "error: not an executable: $binary" >&2
  exit 1
fi

install -d "$bin_dir" "$app_dir" "$icon_dir"
install -m 755 "$binary" "$bin_dir/marker"
install -m 644 "$root/packaging/marker.desktop" "$app_dir/marker.desktop"
install -m 644 "$root/packaging/marker.svg" "$icon_dir/marker.svg"

# Point every Exec= at the installed binary with an absolute path so the
# launcher and desktop actions work even when ~/.local/bin is not on PATH.
sed -i "s|^Exec=marker|Exec=$bin_dir/marker|g" "$app_dir/marker.desktop"

update-desktop-database "$app_dir" 2>/dev/null || true
gtk-update-icon-cache -f -t "$prefix/share/icons/hicolor" 2>/dev/null || true

xdg-mime default marker.desktop application/pdf

echo "Installed: $bin_dir/marker"
echo "Desktop:   $app_dir/marker.desktop"
echo "Default PDF handler: $(xdg-mime query default application/pdf)"
