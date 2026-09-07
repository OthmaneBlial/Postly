// Rasterize the existing website/logo.svg geometry using AppKit. No new design.
// swift tools/render-macos-icon.swift /path/to/Postly.iconset
import AppKit
let args = CommandLine.arguments
guard args.count == 2 else { fatalError("Pass the output .iconset folder") }
let destination = URL(fileURLWithPath: args[1], isDirectory: true)
try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = size * scale
        let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        let transform = NSAffineTransform()
        transform.scale(by: CGFloat(pixels) / 512)
        transform.concat()
        let dark = NSColor(srgbRed: 16/255, green: 19/255, blue: 21/255, alpha: 1)
        dark.setFill()
        NSBezierPath(roundedRect: NSRect(x: 0, y: 0, width: 512, height: 512), xRadius: 116, yRadius: 116).fill()
        let route = NSBezierPath()
        route.move(to: NSPoint(x: 146, y: 126))
        route.line(to: NSPoint(x: 146, y: 386))
        route.line(to: NSPoint(x: 272, y: 386))
        route.curve(to: NSPoint(x: 374, y: 302), controlPoint1: NSPoint(x: 334, y: 386), controlPoint2: NSPoint(x: 374, y: 353))
        route.curve(to: NSPoint(x: 272, y: 218), controlPoint1: NSPoint(x: 374, y: 251), controlPoint2: NSPoint(x: 334, y: 218))
        route.line(to: NSPoint(x: 204, y: 218))
        route.lineWidth = 28; route.lineCapStyle = .round; route.lineJoinStyle = .round
        NSColor(srgbRed: 216/255, green: 243/255, blue: 106/255, alpha: 1).setStroke()
        route.stroke()
        let endpoint = NSColor(srgbRed: 1, green: 118/255, blue: 95/255, alpha: 1)
        endpoint.setStroke(); endpoint.setFill()
        let line = NSBezierPath()
        line.move(to: NSPoint(x: 204, y: 302)); line.line(to: NSPoint(x: 286, y: 302)); line.lineWidth = 18; line.lineCapStyle = .round; line.stroke()
        NSBezierPath(ovalIn: NSRect(x: 345, y: 101, width: 50, height: 50)).fill()
        dark.setFill(); NSBezierPath(ovalIn: NSRect(x: 361, y: 117, width: 18, height: 18)).fill()
        NSGraphicsContext.restoreGraphicsState()
        let suffix = scale == 2 ? "@2x" : ""
        let name = "icon_\(size)x\(size)\(suffix).png"
        try rep.representation(using: .png, properties: [:])!.write(to: destination.appendingPathComponent(name), options: .withoutOverwriting)
    }
}
