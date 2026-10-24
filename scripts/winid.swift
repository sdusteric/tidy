// Usage: winid <pid> [--all]
//        winid --locked       exit 0 when the login session's screen is locked, 1 when not
//        winid --scale        print the main display's backing scale (device pixels per point)
// Prints one line per window of the process:
//   id x y w h onscreen layer alpha scale=<s> name=<name>
// where scale is the backing scale of the display holding the window's center ("none" when
// the window lies outside every display). Uses CoreGraphics only. Never activates anything.
import CoreGraphics
import Foundation

/// Device pixels per point of a display, from its current mode.
func scale(of display: CGDirectDisplayID) -> Double? {
    guard let mode = CGDisplayCopyDisplayMode(display), mode.width > 0 else { return nil }
    return Double(mode.pixelWidth) / Double(mode.width)
}

func fmt(_ s: Double?) -> String {
    guard let s = s else { return "none" }
    return s == s.rounded() ? String(Int(s)) : String(format: "%.2f", s)
}

/// The display whose bounds (global top-left coordinates, as kCGWindowBounds) hold the point.
func display(at p: CGPoint) -> CGDirectDisplayID? {
    var ids = [CGDirectDisplayID](repeating: 0, count: 16)
    var n: UInt32 = 0
    guard CGGetActiveDisplayList(16, &ids, &n) == .success else { return nil }
    return ids.prefix(Int(n)).first { CGDisplayBounds($0).contains(p) }
}

let args = CommandLine.arguments
if args.count >= 2, args[1] == "--locked" {
    let session = CGSessionCopyCurrentDictionary() as? [String: Any] ?? [:]
    let locked = (session["CGSSessionScreenIsLocked"] as? Bool) ?? false
    exit(locked ? 0 : 1)
}
if args.count >= 2, args[1] == "--scale" {
    print(fmt(scale(of: CGMainDisplayID())))
    exit(0)
}
guard args.count >= 2, let pid = Int32(args[1]) else {
    FileHandle.standardError.write("usage: winid <pid> [--all] | --locked | --scale\n".data(using: .utf8)!)
    exit(2)
}
let all = args.contains("--all")
let opts: CGWindowListOption = all ? [.optionAll] : [.optionOnScreenOnly]
guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else { exit(1) }
var n = 0
for w in list {
    guard let owner = w[kCGWindowOwnerPID as String] as? Int32, owner == pid else { continue }
    let id = w[kCGWindowNumber as String] as? Int ?? 0
    let b = w[kCGWindowBounds as String] as? [String: Any] ?? [:]
    let x = b["X"] as? Double ?? 0, y = b["Y"] as? Double ?? 0
    let ww = b["Width"] as? Double ?? 0, hh = b["Height"] as? Double ?? 0
    let on = (w[kCGWindowIsOnscreen as String] as? Bool ?? false) ? 1 : 0
    let layer = w[kCGWindowLayer as String] as? Int ?? -1
    let alpha = w[kCGWindowAlpha as String] as? Double ?? -1
    let name = w[kCGWindowName as String] as? String ?? ""
    let s = display(at: CGPoint(x: x + ww / 2, y: y + hh / 2)).flatMap(scale(of:))
    print("\(id) \(Int(x)) \(Int(y)) \(Int(ww)) \(Int(hh)) onscreen=\(on) layer=\(layer) alpha=\(alpha) scale=\(fmt(s)) name=\(name)")
    n += 1
}
exit(n > 0 ? 0 : 1)
