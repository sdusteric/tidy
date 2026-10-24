// swift make-icon.swift out.png  : placeholder 1024x1024 app icon (squircle + three tidy bars)
import AppKit
let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon-1024.png"
let size = 1024
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8,
    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
// Apple icon grid: 824x824 shape centered in 1024 canvas
let r = NSRect(x: 100, y: 100, width: 824, height: 824)
let path = NSBezierPath(roundedRect: r, xRadius: 184, yRadius: 184)
NSGradient(starting: NSColor(calibratedRed: 0.22, green: 0.78, blue: 0.62, alpha: 1),
           ending: NSColor(calibratedRed: 0.08, green: 0.50, blue: 0.48, alpha: 1))!.draw(in: path, angle: -90)
NSColor.white.setFill()
// Three rounded bars of decreasing length, left aligned: a tidy list.
let bars: [(CGFloat, CGFloat)] = [(240, 544), (240, 420), (240, 296)]
var y: CGFloat = 640
for (x, w) in bars {
    NSBezierPath(roundedRect: NSRect(x: x, y: y, width: w, height: 72), xRadius: 36, yRadius: 36).fill()
    y -= 148
}
NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
