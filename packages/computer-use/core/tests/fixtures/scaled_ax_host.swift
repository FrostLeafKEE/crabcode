// Reproduce Chromium's zoomed AX content under an unscaled native window.
import AppKit

let stateURL = URL(fileURLWithPath: CommandLine.arguments[1])
let zoom = Double(CommandLine.arguments[2])!
let app = NSApplication.shared
app.setActivationPolicy(.regular)
app.finishLaunching()
app.deactivate()

func reportedFrame(_ frame: NSRect) -> NSRect {
    let top = NSScreen.screens[0].frame.maxY
    return NSRect(x: frame.minX / zoom, y: top - (top - frame.minY) / zoom,
                  width: frame.width / zoom, height: frame.height / zoom)
}

final class ContentGroup: NSView {
    var contentsView = false
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .group }
    override func accessibilityChildren() -> [Any]? { subviews }
    override func accessibilityLabel() -> String? { contentsView ? "ContentsView" : "Zoom root" }
    override func accessibilityFrame() -> NSRect { reportedFrame(window!.frame) }
}

final class ZoomButton: NSButton {
    override func accessibilityFrame() -> NSRect { reportedFrame(super.accessibilityFrame()) }
}

final class Counter: NSObject {
    var upper = 0
    var lower = 0
    @objc func press(_ sender: NSButton) {
        if sender.tag == 1 { upper += 1 } else { lower += 1 }
    }
}

let window = NSWindow(contentRect: NSRect(x: 260, y: 200, width: 640, height: 700),
                      styleMask: [.titled, .closable], backing: .buffered, defer: false)
window.title = "CrabCode scaled AX fixture"
window.isReleasedWhenClosed = false
let root = ContentGroup(frame: window.contentView!.bounds)
let contents = ContentGroup(frame: root.bounds)
contents.contentsView = true
root.addSubview(contents)
window.contentView = root
let counter = Counter()
let upper = ZoomButton(title: "Upper", target: counter, action: #selector(Counter.press(_:)))
upper.frame = NSRect(x: 100, y: 180, width: 180, height: 50)
upper.tag = 1
let lower = ZoomButton(title: "Lower", target: counter, action: #selector(Counter.press(_:)))
lower.frame = NSRect(x: 100, y: 100, width: 180, height: 50)
lower.tag = 2
contents.addSubview(upper)
contents.addSubview(lower)
window.orderBack(nil)

func point(_ view: NSView) -> [Double] {
    let rect = window.convertToScreen(view.convert(view.bounds, to: nil))
    return [rect.midX, NSScreen.screens[0].frame.maxY - rect.midY]
}

func recordState() {
    let state: [String: Any] = [
        "pid": ProcessInfo.processInfo.processIdentifier,
        "window_id": window.windowNumber,
        "upper_point": point(upper), "lower_point": point(lower),
        "close_point": point(window.standardWindowButton(.closeButton)!),
        "upper_presses": counter.upper, "lower_presses": counter.lower,
    ]
    try! JSONSerialization.data(withJSONObject: state).write(to: stateURL, options: .atomic)
}

while true {
    if let event = app.nextEvent(matching: .any, until: Date(timeIntervalSinceNow: 0.05),
                                 inMode: .default, dequeue: true) {
        app.sendEvent(event)
    }
    _ = RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.01))
    window.displayIfNeeded()
    recordState()
}
