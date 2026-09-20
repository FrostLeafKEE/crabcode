// Isolated integration fixture: two overlapping windows owned by one process.
// Records actual scroll offsets without activating the app or moving the pointer.
import AppKit

let stateURL = URL(fileURLWithPath: CommandLine.arguments[1])
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
var receivedWheelEvents = 0
var receivedWindow = -1
var receivedPoint = NSPoint.zero
let monitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { event in
    receivedWheelEvents += 1
    receivedWindow = event.windowNumber
    receivedPoint = event.locationInWindow
    return event
}

final class Document: NSView {
    override var isFlipped: Bool { true }
    override func draw(_ dirtyRect: NSRect) {
        NSColor.white.setFill()
        dirtyRect.fill()
        for row in 0..<100 {
            ("CrabCode test row \(row)" as NSString).draw(
                at: NSPoint(x: 20, y: row * 40),
                withAttributes: [.foregroundColor: NSColor.black])
        }
    }
}

final class ScrollArea: NSScrollView {
    var wheelEvents = 0
    override func scrollWheel(with event: NSEvent) {
        wheelEvents += 1
        super.scrollWheel(with: event)
    }
}

func makeWindow(_ title: String) -> (NSWindow, ScrollArea) {
    let window = NSWindow(contentRect: NSRect(x: 120, y: 200, width: 640, height: 420),
                          styleMask: [.titled, .closable], backing: .buffered, defer: false)
    window.title = title
    window.isReleasedWhenClosed = false
    let scroll = ScrollArea(frame: window.contentView!.bounds)
    scroll.documentView = Document(frame: NSRect(x: 0, y: 0, width: 640, height: 4000))
    scroll.hasVerticalScroller = true
    window.contentView = scroll
    scroll.contentView.scroll(to: NSPoint(x: 0, y: 500))
    scroll.reflectScrolledClipView(scroll.contentView)
    return (window, scroll)
}

let (target, targetScroll) = makeWindow("CrabCode scroll target")
let (decoy, decoyScroll) = makeWindow("CrabCode scroll decoy")
target.orderBack(nil)
decoy.order(.above, relativeTo: target.windowNumber)

func recordState() {
    let rect = target.convertToScreen(targetScroll.convert(targetScroll.bounds, to: nil))
    let top = NSScreen.screens[0].frame.maxY
    let cursor = CGEvent(source: nil)!.location
    let state: [String: Any] = [
        "pid": ProcessInfo.processInfo.processIdentifier,
        "target_id": target.windowNumber,
        "decoy_id": decoy.windowNumber,
        "x": Int(rect.midX), "y": Int(top - rect.midY),
        "origin_x": Int(target.frame.minX), "origin_y": Int(top - target.frame.maxY),
        "target_offset": targetScroll.contentView.bounds.origin.y,
        "decoy_offset": decoyScroll.contentView.bounds.origin.y,
        "target_events": targetScroll.wheelEvents,
        "decoy_events": decoyScroll.wheelEvents,
        "received_events": receivedWheelEvents,
        "received_window": receivedWindow,
        "received_point": [receivedPoint.x, receivedPoint.y],
        "active": app.isActive,
        "frontmost_pid": NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1,
        "cursor": [cursor.x, cursor.y],
    ]
    try! JSONSerialization.data(withJSONObject: state).write(to: stateURL, options: .atomic)
}

// Pump events without NSApplication.run's automatic launch activation.
while true {
    if let event = app.nextEvent(matching: .any, until: Date(timeIntervalSinceNow: 0.05),
                                 inMode: .default, dequeue: true) {
        app.sendEvent(event)
    }
    recordState()
}
