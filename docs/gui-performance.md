# Native GUI performance protocol

The [CLI/core measurements](measurements/2026-09-08-validation.md) do not measure
the desktop's startup, idle memory or interaction latency. Those roadmap gates
require the running native application and an unlocked graphical session.

The [first completed native baseline](measurements/2026-09-08-macos-gui.md)
includes five real process launches, idle RSS, broad search and result-list
navigation, with raw samples, captures and explicit observation limits.

## Reproducible large workspace

```bash
# Choose a new directory. The generator refuses a nonempty destination.
node tools/generate-gui-benchmark.mjs tmp/gui-performance-workspace
./target/aarch64-apple-darwin/release/postly validate \
  tmp/gui-performance-workspace --output-json
# Launch the exact release GUI under test on that same directory.
./target/aarch64-apple-darwin/release/postly-gui tmp/gui-performance-workspace
```

Adjust only the executable path to the actual tested release. The fixture has
one collection, 10,000 distinct request IDs and no environment or credentials.
URLs use loopback port 1; **do not press Send**. Search for `09999` and open
**Last request**. The editor URL must end in `/request/09999`. Search by the
broader `request` term as well before drawing conclusions about large result
lists or scrolling; a single-match test does not cover those interactions.

Generator validation on 8 September 2026: the actual CLI reported one
collection, 10,000 requests, zero environments and no validation issues.
Request IDs were independently counted as unique. Rerunning the generator on
the same nonempty directory failed without changing any request bytes.

## Measurements to retain

- Exact binary hash, source commit, build profile, target, machine, OS and
  window size; note background load and whether caches/UI state are retained.
- Five process launches: time to WindowServer registration and time to first
  visibly usable content, as separate values. Registration is not first paint.
- Idle process RSS after at least five seconds without interaction, with raw
  samples and sampling intervals. This is not a peak or memory-leak test.
- Search, result selection and scrolling with the 10,000-request fixture.
  Keep evidence of the expected results and all raw observation times. If
  screenshots/OCR are used, report capture/polling overhead and observation
  bounds rather than presenting them as exact application frame times.
- Recheck keyboard focus, selection and reachability after any optimization.

## External macOS observer

The observer operates on the **real native executable**, not a simulated app
or an in-process egui test. Compile the Swift helper before measuring:

```bash
swiftc tools/measure-macos-gui.swift -o /tmp/postly-gui-probe
/tmp/postly-gui-probe \
  /absolute/path/to/Postly.app/Contents/MacOS/postly-gui \
  /absolute/path/to/gui-performance-workspace \
  /absolute/path/to/a-new-measurement-directory 5
```

The output directory must not exist. The observer requires Accessibility and
Screen Recording permissions and an unlocked session. It launches only the
specified binary with the generated workspace, captures only its window,
identifies visible text using Apple's Vision OCR and drives search, selection,
a 10,000-result search and a scroll-to-bottom event. Do not use the computer
during measurement. It refuses input if another app is foreground, and ends
only its own child process after each run; existing apps are not terminated.

`report.json` retains every completed sample, requested/completed run counts,
binary hashes and timestamps. An incomplete report is not a completed run.
PNGs and app logs remain beside it for visual inspection. Inspect the frames
before publishing a result: OCR is an observer, not proof that every UI
interaction or accessibility behavior is correct. The one-event query insertion
does not measure per-keystroke latency, and a large synthetic wheel event does
not measure continuous scrolling frame rate.

## Current validation boundary

The initial native capture attempt on 8 September failed because macOS reported
`CGSSessionScreenIsLocked: true`. Window capture failed and a display capture
was black. No GUI timing/RSS result from that attempt is published as valid.
After the session became unlocked, the real app's 10,000-request workspace,
unique search result, selected URL and broad-result scroll were captured and
inspected. The original user app was left running. Failed pilot runs stay
separate from completed measurements. Do not unlock sessions programmatically
or count a capture failure as a passed visual check.
