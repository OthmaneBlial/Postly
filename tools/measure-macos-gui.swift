// External native-GUI measurement. Run only with the fictional generated workspace.
// swiftc tools/measure-macos-gui.swift -o /tmp/postly-gui-probe
// /tmp/postly-gui-probe GUI_BINARY WORKSPACE NEW_OUTPUT_DIRECTORY RUNS
import AppKit
import ApplicationServices
import CryptoKit
import Vision

enum ProbeError: Error { case failed(String) }
func main() throws {
  let args = CommandLine.arguments
  guard args.count == 5, let runs = Int(args[4]), (1...5).contains(runs) else {
    throw ProbeError.failed("Usage: probe GUI_BINARY WORKSPACE NEW_OUTPUT_DIRECTORY RUNS(1...5)")
  }
  let binary = URL(fileURLWithPath: args[1]).standardizedFileURL
  let workspace = URL(fileURLWithPath: args[2]).standardizedFileURL
  let output = URL(fileURLWithPath: args[3]).standardizedFileURL
  let fm = FileManager.default
  var measuredPID: pid_t?
  let session = CGSessionCopyCurrentDictionary() as? [String: Any]
  guard (session?["CGSSessionScreenIsLocked"] as? Bool) != true else {
    throw ProbeError.failed("Unlock the macOS session before measuring; no measurements taken")
  }
  guard AXIsProcessTrusted(), CGPreflightScreenCaptureAccess() else {
    throw ProbeError.failed(
      "Accessibility and Screen Recording permissions are required; no measurements taken")
  }
  guard !fm.fileExists(atPath: output.path) else {
    throw ProbeError.failed("Output directory already exists")
  }
  let requestDirectory = workspace.appendingPathComponent("collections/benchmark/requests")
  guard try fm.contentsOfDirectory(atPath: requestDirectory.path).count == 10000,
    try String(contentsOf: workspace.appendingPathComponent("postly.toml"), encoding: .utf8)
      .contains("GUI benchmark — fictional data")
  else {
    throw ProbeError.failed(
      "Use tools/generate-gui-benchmark.mjs; do not measure a private workspace")
  }
  try fm.createDirectory(at: output, withIntermediateDirectories: true)

  func now() -> Double { ProcessInfo.processInfo.systemUptime }
  func command(_ executable: String, _ arguments: [String]) throws -> String {
    let task = Process()
    task.executableURL = URL(fileURLWithPath: executable)
    task.arguments = arguments
    let pipe = Pipe()
    task.standardOutput = pipe
    task.standardError = FileHandle.standardError
    try task.run()
    let data = pipe.fileHandleForReading.readDataToEndOfFile()
    task.waitUntilExit()
    guard task.terminationStatus == 0 else {
      throw ProbeError.failed("Command failed: \(executable)")
    }
    return String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
  }
  func window(_ pid: pid_t) -> (UInt32, CGRect)? {
    let windows =
      CGWindowListCopyWindowInfo(.optionOnScreenOnly, kCGNullWindowID) as? [[String: Any]] ?? []
    for item in windows
    where (item[kCGWindowOwnerPID as String] as? Int32) == pid
      && (item[kCGWindowLayer as String] as? Int) == 0
    {
      guard let id = item[kCGWindowNumber as String] as? UInt32,
        let bounds = item[kCGWindowBounds as String] as? NSDictionary,
        let rect = CGRect(dictionaryRepresentation: bounds), rect.width >= 600, rect.height >= 400
      else { continue }
      return (id, rect)
    }
    return nil
  }
  struct TextBox {
    let text: String
    let rect: CGRect
  }
  func requireMeasuredForeground() throws {
    guard let pid = measuredPID,
      NSWorkspace.shared.frontmostApplication?.processIdentifier == pid
    else {
      throw ProbeError.failed("Another app is in the foreground; refusing to send input")
    }
  }
  func capture(_ id: UInt32, _ file: URL) throws -> (Double, [TextBox]) {
    _ = try command("/usr/sbin/screencapture", ["-x", "-o", "-l", String(id), file.path])
    let capturedAt = now()
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.recognitionLanguages = ["en-US"]
    request.usesLanguageCorrection = false
    try VNImageRequestHandler(url: file).perform([request])
    let boxes = (request.results ?? []).compactMap { observation -> TextBox? in
      guard let text = observation.topCandidates(1).first?.string else { return nil }
      return TextBox(text: text, rect: observation.boundingBox)
    }
    return (capturedAt, boxes)
  }
  func click(_ box: TextBox, _ bounds: CGRect) throws {
    try requireMeasuredForeground()
    let point = CGPoint(
      x: bounds.minX + box.rect.midX * bounds.width,
      y: bounds.minY + (1 - box.rect.midY) * bounds.height)
    guard
      let down = CGEvent(
        mouseEventSource: nil, mouseType: .leftMouseDown, mouseCursorPosition: point,
        mouseButton: .left),
      let up = CGEvent(
        mouseEventSource: nil, mouseType: .leftMouseUp, mouseCursorPosition: point,
        mouseButton: .left)
    else {
      throw ProbeError.failed("Cannot create pointer events")
    }
    down.post(tap: .cghidEventTap)
    up.post(tap: .cghidEventTap)
  }
  func typeQuery(_ query: String) throws {
    try requireMeasuredForeground()
    // Unicode text, no keyboard-layout-dependent command shortcuts.
    let characters = Array(query.utf16)
    guard let down = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: true),
      let up = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: false)
    else {
      throw ProbeError.failed("Cannot create keyboard events")
    }
    characters.withUnsafeBufferPointer { ptr in
      down.keyboardSetUnicodeString(stringLength: ptr.count, unicodeString: ptr.baseAddress)
      up.keyboardSetUnicodeString(stringLength: ptr.count, unicodeString: ptr.baseAddress)
    }
    down.post(tap: .cghidEventTap)
    up.post(tap: .cghidEventTap)
  }
  struct Observation {
    let elapsed: Double
    let boxes: [TextBox]
  }
  func observe(
    _ task: Process, _ id: UInt32, _ started: Double, _ filename: String,
    _ ready: ([TextBox]) -> Bool
  ) throws -> Observation {
    var attempts = 0
    while now() - started < 20 {
      guard task.isRunning else { throw ProbeError.failed("GUI exited before \(filename)") }
      // A window can be listed before WindowServer can capture its first frame.
      if let (timestamp, boxes) = try? capture(
        window(task.processIdentifier)?.0 ?? id,
        output.appendingPathComponent("\(filename)-\(attempts).png")), ready(boxes)
      {
        return Observation(elapsed: (timestamp - started) * 1000, boxes: boxes)
      }
      attempts += 1
      Thread.sleep(forTimeInterval: 0.1)
    }
    throw ProbeError.failed("Timed out observing \(filename); inspect retained screenshots")
  }
  var samples: [[String: Any]] = []
  for run in 1...runs {
    // A new process per run; caches and the workspace's private UI state are retained.
    let task = Process()
    task.executableURL = binary
    task.arguments = [workspace.path]
    let log = output.appendingPathComponent("run-\(run).log")
    guard fm.createFile(atPath: log.path, contents: nil) else {
      throw ProbeError.failed("Cannot create GUI log")
    }
    let handle = try FileHandle(forWritingTo: log)
    task.standardOutput = handle
    task.standardError = handle
    let start = now()
    try task.run()
    measuredPID = task.processIdentifier
    defer {
      // Terminate only the child created for this sample, never an existing user app.
      if task.isRunning {
        task.terminate()
        task.waitUntilExit()
      }
      try? handle.close()
    }
    var found: (UInt32, CGRect)?
    while now() - start < 20, task.isRunning {
      if let current = window(task.processIdentifier) {
        found = current
        break
      }
      Thread.sleep(forTimeInterval: 0.01)
    }
    guard let (id, bounds) = found else {
      throw ProbeError.failed("No native window within 20 seconds")
    }
    let windowMs = (now() - start) * 1000
    NSRunningApplication(processIdentifier: task.processIdentifier)?.activate(options: [
      .activateAllWindows
    ])
    let initial = try observe(task, id, start, "run-\(run)-startup") { boxes in
      boxes.contains { $0.text.contains("Find a request") }
        && boxes.contains { $0.text.contains("Send") }
    }
    guard let search = initial.boxes.first(where: { $0.text.contains("Find a request") }) else {
      throw ProbeError.failed("Search field not identified")
    }
    Thread.sleep(forTimeInterval: 5)
    var rss: [Int] = []
    for _ in 0..<3 {
      guard
        let value = Int(
          try command("/bin/ps", ["-o", "rss=", "-p", String(task.processIdentifier)]))
      else {
        throw ProbeError.failed("Could not measure child RSS")
      }
      rss.append(value)
      Thread.sleep(forTimeInterval: 1)
    }
    try click(search, bounds)
    Thread.sleep(forTimeInterval: 0.1)
    let queryStarted = now()
    try typeQuery("09999")
    let matches = try observe(task, id, queryStarted, "run-\(run)-search") { boxes in
      boxes.contains { $0.text.contains("Last request") && $0.rect.midX < 0.3 }
    }
    guard
      let result = matches.boxes.first(where: {
        $0.text.contains("Last request") && $0.rect.midX < 0.3
      })
    else {
      throw ProbeError.failed("Search result not identified")
    }
    let selectStarted = now()
    try click(result, bounds)
    let selected = try observe(task, id, selectStarted, "run-\(run)-selection") { boxes in
      boxes.contains { $0.text.contains("09999") && $0.rect.midX > 0.3 }
        && boxes.contains { $0.text.contains("Find a request") }
    }
    guard let emptySearch = selected.boxes.first(where: { $0.text.contains("Find a request") })
    else {
      throw ProbeError.failed("Search did not clear after selection")
    }
    try click(emptySearch, bounds)
    Thread.sleep(forTimeInterval: 0.1)
    let broadStarted = now()
    try typeQuery("request")
    let broad = try observe(task, id, broadStarted, "run-\(run)-broad-search") { boxes in
      boxes.contains { $0.text.filter { !$0.isWhitespace && $0 != "," }.contains("10000matching") }
        && boxes.contains { $0.text.hasPrefix("GET Request 00000") && $0.rect.midX < 0.3 }
    }
    guard
      let firstResult = broad.boxes.first(where: {
        $0.text.hasPrefix("GET Request 00000") && $0.rect.midX < 0.3
      })
    else {
      throw ProbeError.failed("First broad result not found")
    }
    let scrollPoint = CGPoint(
      x: bounds.minX + firstResult.rect.midX * bounds.width,
      y: bounds.minY + (1 - firstResult.rect.midY) * bounds.height)
    guard
      let move = CGEvent(
        mouseEventSource: nil, mouseType: .mouseMoved,
        mouseCursorPosition: scrollPoint, mouseButton: .left),
      let wheel = CGEvent(
        scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1,
        wheel1: -1_000_000, wheel2: 0, wheel3: 0)
    else {
      throw ProbeError.failed("Cannot create scroll events")
    }
    try requireMeasuredForeground()
    move.post(tap: .cghidEventTap)
    Thread.sleep(forTimeInterval: 0.1)
    let scrollStarted = now()
    try requireMeasuredForeground()
    wheel.location = scrollPoint
    wheel.post(tap: .cghidEventTap)
    let scrolled = try observe(task, id, scrollStarted, "run-\(run)-scroll-bottom") { boxes in
      boxes.contains { $0.text.contains("Last request") && $0.rect.midX < 0.3 }
    }
    let sample: [String: Any] = [
      "run": run, "pid": task.processIdentifier,
      "window_ms": windowMs, "startup_content_observed_by_ms": initial.elapsed,
      "idle_rss_kib": rss, "search_result_observed_by_ms": matches.elapsed,
      "selection_observed_by_ms": selected.elapsed,
      "broad_search_observed_by_ms": broad.elapsed,
      "scroll_bottom_observed_by_ms": scrolled.elapsed,
      "window_points": [bounds.width, bounds.height],
    ]
    samples.append(sample)
    let report: [String: Any] = [
      "binary": binary.path,
      "binary_sha256": SHA256.hash(data: try Data(contentsOf: binary)).map {
        String(format: "%02x", $0)
      }.joined(),
      "observer_binary_sha256": SHA256.hash(
        data: try Data(contentsOf: URL(fileURLWithPath: args[0]))
      ).map {
        String(format: "%02x", $0)
      }.joined(),
      "measured_at_utc": ISO8601DateFormatter().string(from: Date()),
      "os": ProcessInfo.processInfo.operatingSystemVersionString,
      "hardware": try command("/usr/sbin/sysctl", ["-n", "hw.model"]),
      "workspace": workspace.path, "request_count": 10000,
      "requested_runs": runs, "completed_runs": samples.count,
      "method":
        "CGWindow polling 10ms; captured-content OCR upper bounds (capture completion time, excluding later OCR); idle RSS approximately 5/6/7 seconds after startup-content recognition; no cache flushing; no HTTP requests; each query inserted as one Unicode event; scroll-to-bottom is one synthetic pixel-wheel event, not a scroll-FPS test",
      "samples": samples,
    ]
    try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
      .write(to: output.appendingPathComponent("report.json"), options: .atomic)
    print(
      "Completed run \(run)/\(runs): window \(Int(windowMs)) ms, search observed by \(Int(matches.elapsed)) ms"
    )
    fflush(stdout)
  }
}
do { try main() } catch {
  fputs("GUI measurement failed: \(error)\n", stderr)
  exit(1)
}
