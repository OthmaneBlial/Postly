#!/bin/sh
# Replay a packaged archive without Cargo, Node or repository state.
# Usage: tools/replay-package.sh /path/to/postly-vX.Y.Z-OS-ARCH.tar.gz
# Set POSTLY_SKIP_GUI=1 only for a deliberately headless CLI replay.
set -eu

if [ "$#" -ne 1 ]; then
  echo "Usage: $0 PACKAGE_TAR_GZ" >&2
  exit 64
fi

archive=$1
case "$archive" in
  /*) ;;
  *) archive="$(pwd)/$archive" ;;
esac
[ -f "$archive" ] || {
  echo "package archive not found: $archive" >&2
  exit 66
}

root=$(mktemp -d "${TMPDIR:-/tmp}/postly-package-replay.XXXXXX")
demo_pid=
gui_pid=

cleanup() {
  if [ -n "${gui_pid:-}" ]; then
    kill -TERM "$gui_pid" 2>/dev/null || true
    wait "$gui_pid" 2>/dev/null || true
  fi
  if [ -n "${demo_pid:-}" ]; then
    kill -TERM "$demo_pid" 2>/dev/null || true
    wait "$demo_pid" 2>/dev/null || true
  fi
  rm -rf "$root"
}
trap cleanup EXIT HUP INT TERM

extract="$root/extracted"
mkdir "$extract"
tar -xzf "$archive" -C "$extract"
package_dir=$(find "$extract" -mindepth 1 -maxdepth 1 -type d -print -quit)
[ -n "$package_dir" ] || {
  echo "archive does not contain a top-level package directory" >&2
  exit 65
}

cli="$package_dir/postly"
gui="$package_dir/postly-gui"
[ -x "$cli" ] && [ -x "$gui" ] || {
  echo "package must contain executable postly and postly-gui files" >&2
  exit 65
}

verify_checksums() {
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$package_dir" && sha256sum -c SHA256SUMS)
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$package_dir" && shasum -a 256 -c SHA256SUMS)
  else
    echo "sha256sum or shasum is required to verify SHA256SUMS" >&2
    exit 69
  fi
}

verify_checksums >/dev/null
version=$(
  sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$package_dir/postly-package.json" | head -n 1
)
[ -n "$version" ] || {
  echo "package manifest does not contain a version" >&2
  exit 65
}
"$cli" --version | grep -Fx "postly $version" >/dev/null
"$cli" --help >/dev/null

workspace="$root/orders"
demo_log="$root/demo.log"
"$cli" demo "$workspace" --port 0 >"$demo_log" 2>&1 &
demo_pid=$!
i=0
while [ ! -f "$workspace/postly.toml" ]; do
  if ! kill -0 "$demo_pid" 2>/dev/null; then
    cat "$demo_log" >&2
    echo "packaged demo server exited before creating its workspace" >&2
    exit 70
  fi
  i=$((i + 1))
  if [ "$i" -ge 100 ]; then
    cat "$demo_log" >&2
    echo "packaged demo workspace was not created within 10 seconds" >&2
    exit 70
  fi
  sleep 0.1
done

"$cli" validate "$workspace" --output-json | grep '"valid": true' >/dev/null
run_report="$root/run.json"
"$cli" run "$workspace" --reporter json >"$run_report"
grep '"passed": 2' "$run_report" >/dev/null
grep '"failed": 0' "$run_report" >/dev/null
grep '"assertions": 5' "$run_report" >/dev/null

kill -TERM "$demo_pid" 2>/dev/null || true
wait "$demo_pid" 2>/dev/null || true
demo_pid=

if [ "${POSTLY_SKIP_GUI:-0}" != "1" ]; then
  gui_log="$root/gui.log"
  "$gui" "$workspace" >"$gui_log" 2>&1 &
  gui_pid=$!
  sleep 3
  if ! kill -0 "$gui_pid" 2>/dev/null; then
    cat "$gui_log" >&2
    echo "packaged GUI exited before the three-second smoke window" >&2
    exit 71
  fi
  kill -TERM "$gui_pid" 2>/dev/null || true
  wait "$gui_pid" 2>/dev/null || true
  gui_pid=
fi

echo "package replay: PASS"
echo "archive: $archive"
echo "version: $version"
echo "checksums, CLI version/help, loopback demo, validation and run passed"
if [ "${POSTLY_SKIP_GUI:-0}" = "1" ]; then
  echo "GUI smoke: SKIPPED (POSTLY_SKIP_GUI=1)"
else
  echo "GUI smoke: process stayed alive for three seconds"
fi
