#!/bin/sh
# Verify that replacing a previous CLI/app with a preview preserves a local project.
# Usage: tools/verify-update-preservation.sh OLD_TAR_GZ NEW_TAR_GZ
set -eu

if [ "$#" -ne 2 ]; then
  echo "Usage: $0 OLD_TAR_GZ NEW_TAR_GZ" >&2
  exit 64
fi
old_archive=$1
new_archive=$2
[ -f "$old_archive" ] || { echo "old archive not found: $old_archive" >&2; exit 66; }
[ -f "$new_archive" ] || { echo "new archive not found: $new_archive" >&2; exit 66; }

root=$(mktemp -d "${TMPDIR:-/tmp}/postly-update-verify.XXXXXX")
trap 'rm -rf "$root"' EXIT HUP INT TERM
old_root="$root/old"
new_root="$root/new"
mkdir "$old_root" "$new_root"
tar -xzf "$old_archive" -C "$old_root"
tar -xzf "$new_archive" -C "$new_root"
old_dir=$(find "$old_root" -mindepth 1 -maxdepth 1 -type d -print -quit)
new_dir=$(find "$new_root" -mindepth 1 -maxdepth 1 -type d -print -quit)
old_cli="$old_dir/postly"
new_cli="$new_dir/postly"
new_gui="$new_dir/postly-gui"
[ -x "$old_cli" ] && [ -x "$new_cli" ] && [ -x "$new_gui" ] || {
  echo "archives must contain executable postly and postly-gui files" >&2
  exit 65
}

workspace="$root/workspace"
"$old_cli" init --name "Upgrade fixture" "$workspace" >/dev/null
"$old_cli" new request --workspace "$workspace" --collection Upgrade \
  --name Health http://127.0.0.1:1/health >/dev/null
mkdir -p "$workspace/.postly"
node --input-type=module -e '
  import fs from "node:fs";
  const file = process.argv[1] + "/.postly/gui-settings.json";
  fs.writeFileSync(file, JSON.stringify({timeout_seconds:45,max_redirects:4,max_response_megabytes:256,proxy_url:"",no_proxy_hosts:"",ca_cert_path:"",client_identity_path:"",certificate_associations:[],insecure_tls:false,theme:"light"}, null, 2) + "\n", {flag:"wx"});
' "$workspace"

snapshot() {
  # Recovery, tabs and cookie files are runtime state; canonical collections,
  # environments and the project manifest are the update-preservation contract.
  find "$workspace" -type f -not -path "$workspace/.postly/*" -print | LC_ALL=C sort | while IFS= read -r file; do
    shasum -a 256 "$file"
  done
}
snapshot > "$root/before.sha256"
before_settings=$(shasum -a 256 "$workspace/.postly/gui-settings.json")

"$old_cli" list "$workspace" >/dev/null
"$new_cli" --version | grep -Fx 'postly 0.2.0-preview.1' >/dev/null
"$new_cli" list "$workspace" >/dev/null
"$new_cli" search --workspace "$workspace" Health >/dev/null
"$new_cli" validate "$workspace" --output-json | grep '"valid": true' >/dev/null

# Launch only the newly extracted child. A GUI launch is part of the preservation
# check; it is skipped only when POSTLY_SKIP_GUI=1 is explicitly supplied.
if [ "${POSTLY_SKIP_GUI:-0}" != "1" ]; then
  "$new_gui" "$workspace" >"$root/gui.log" 2>&1 &
  gui_pid=$!
  sleep 3
  kill -TERM "$gui_pid" 2>/dev/null || true
  wait "$gui_pid" 2>/dev/null || true
fi

snapshot > "$root/after.sha256"
diff -u "$root/before.sha256" "$root/after.sha256" >/dev/null
after_settings=$(shasum -a 256 "$workspace/.postly/gui-settings.json")
[ "$before_settings" = "$after_settings" ]
echo "update preservation: PASS"
echo "old: $old_cli"
echo "new: $new_cli"
echo "workspace files and GUI preferences unchanged"
