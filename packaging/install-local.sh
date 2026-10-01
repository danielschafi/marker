#!/usr/bin/env bash
# Install Marker into the user prefix for the Omarchy/desktop app picker.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
prefix="${PREFIX:-$HOME/.local}"
bin_dir="$prefix/bin"
app_dir="$prefix/share/applications"
icon_dir="$prefix/share/icons/hicolor/scalable/apps"

binary="${1:-}"
if [[ -z "$binary" ]]; then
  if [[ -x "$root/target/release/marker" ]]; then
    binary="$root/target/release/marker"
  else
    echo "usage: $0 [path-to-marker-binary]" >&2
    echo "or build first: cargo build --release" >&2
    exit 1
  fi
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
