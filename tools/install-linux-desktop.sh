#!/bin/sh
# Install a packaged Postly GUI and its canonical icon for the current user.
# Usage: tools/install-linux-desktop.sh /path/to/postly-vX.Y.Z-linux-x86_64
set -eu

if [ "$#" -ne 1 ]; then
  echo "Usage: $0 PACKAGE_DIRECTORY" >&2
  exit 64
fi

package_dir=$1
case "$package_dir" in
  /*) ;;
  *) package_dir="$(pwd)/$package_dir" ;;
esac

icon_name=io.github.othmaneblial.postly
desktop_source="$package_dir/postly.desktop"
icon_source="$package_dir/$icon_name.svg"
gui_binary="$package_dir/postly-gui"
[ -f "$desktop_source" ] || {
  echo "package desktop entry not found: $desktop_source" >&2
  exit 66
}
[ -f "$icon_source" ] || {
  echo "package icon not found: $icon_source" >&2
  exit 66
}
[ -x "$gui_binary" ] || {
  echo "package GUI executable not found or not executable: $gui_binary" >&2
  exit 66
}
grep -Fx "Icon=$icon_name" "$desktop_source" >/dev/null || {
  echo "desktop entry does not reference the canonical postly icon name" >&2
  exit 65
}

user_home=${HOME:?HOME must be set for a user installation}
data_home=${XDG_DATA_HOME:-"$user_home/.local/share"}
icon_dir="$data_home/icons/hicolor/scalable/apps"
application_dir="$data_home/applications"
mkdir -p "$icon_dir" "$application_dir"

# Desktop Exec values are quoted so paths containing spaces remain launchable.
escape_desktop_value() {
  printf '%s' "$1" | sed 's/[\\\"]/\\&/g'
}
escaped_exec=$(escape_desktop_value "$gui_binary")
escaped_path=$(escape_desktop_value "$package_dir")
desktop_tmp=$(mktemp "${TMPDIR:-/tmp}/postly-desktop.XXXXXX")
cleanup() {
  rm -f "$desktop_tmp"
}
trap cleanup EXIT HUP INT TERM

awk -v exec_path="$escaped_exec" -v work_path="$escaped_path" '
  BEGIN { exec_seen = 0; path_seen = 0 }
  /^Exec=/ { print "Exec=\"" exec_path "\""; exec_seen = 1; next }
  /^Path=/ { print "Path=\"" work_path "\""; path_seen = 1; next }
  { print }
  END {
    if (!path_seen) print "Path=\"" work_path "\""
    if (!exec_seen) exit 1
  }
' "$desktop_source" >"$desktop_tmp" || {
  echo "desktop entry is missing an Exec field" >&2
  exit 65
}

install -m 644 "$icon_source" "$icon_dir/$icon_name.svg"
install -m 644 "$desktop_tmp" "$application_dir/postly.desktop"
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$application_dir" >/dev/null 2>&1 || true
fi

echo "Postly desktop integration installed for this user."
echo "launcher: $application_dir/postly.desktop"
echo "icon: $icon_dir/$icon_name.svg"
echo "executable: $gui_binary"
