// Isolated integration fixture: two overlapping windows owned by one process.
// Records pointer delivery, activation, and window ordering throughout a gesture.
import AppKit

let stateURL = URL(fileURLWithPath: CommandLine.arguments[1])
let app = NSApplication.shared
app.setActivationPolicy(.regular)
// The test launches this bundle with open -g, so finishing launch initializes
// accessibility without requesting the user's foreground application slot.
app.finishLaunching()
app.deactivate()
var receivedWheelEvents = 0
var receivedWindow = -1
var receivedPoint = NSPoint.zero
var activations = 0
var everFrontmost = false
var everRaised = false
let activationObserver = NotificationCenter.default.addObserver(
    forName: NSApplication.didBecomeActiveNotification, object: app, queue: nil
) { _ in activations += 1 }
let monitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { event in
    receivedWheelEvents += 1
    receivedWindow = event.windowNumber
    receivedPoint = event.locationInWindow
    return event
}

final class Document: NSView {
    var clicks = 0
    var clickCounts: [Int] = []
    var clickTimes: [Double] = []
    var modifiers: [UInt] = []
    var otherClicks = 0
    var drags = 0
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func mouseDown(with event: NSEvent) {
        clicks += 1
        clickCounts.append(event.clickCount)
        clickTimes.append(event.timestamp)
        modifiers.append(event.modifierFlags.intersection(.deviceIndependentFlagsMask).rawValue)
    }
    override func rightMouseDown(with event: NSEvent) { otherClicks += 1 }
    override func otherMouseDown(with event: NSEvent) { otherClicks += 1 }
    override func mouseDragged(with event: NSEvent) { drags += 1 }
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

final class ButtonCounter: NSObject {
    var presses = 0
    var activateOnPress = false
    @objc func press(_ sender: Any?) {
        presses += 1
        (sender as? NSButton)?.title = "Pressed \(presses)"
        if activateOnPress {
            // LaunchServices explicitly activates this background-launched
            // fixture; AppKit's activate() can be declined on recent macOS.
            let activation = Process()
            activation.executableURL = URL(fileURLWithPath: "/usr/bin/open")
            activation.arguments = ["-a", Bundle.main.bundlePath]
            try! activation.run()
            (sender as? NSButton)?.window?.makeKeyAndOrderFront(nil)
        }
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
let buttonCounter = ButtonCounter()
let button = NSButton(title: "Test click", target: buttonCounter, action: #selector(ButtonCounter.press(_:)))
button.frame = NSRect(x: 20, y: 20, width: 140, height: 32)
targetScroll.addSubview(button)
let decoyButtonCounter = ButtonCounter()
let decoyButton = NSButton(title: "AX click", target: decoyButtonCounter, action: #selector(ButtonCounter.press(_:)))
decoyButton.frame = NSRect(x: 20, y: 20, width: 140, height: 32)
decoyScroll.addSubview(decoyButton)
let activatingButtonCounter = ButtonCounter()
activatingButtonCounter.activateOnPress = true
let activatingButton = NSButton(title: "Activate", target: activatingButtonCounter,
                                action: #selector(ButtonCounter.press(_:)))
activatingButton.frame = NSRect(x: 200, y: 20, width: 140, height: 32)
decoyScroll.addSubview(activatingButton)
target.orderBack(nil)
decoy.order(.above, relativeTo: target.windowNumber)

func recordState() {
    let rect = target.convertToScreen(targetScroll.convert(targetScroll.bounds, to: nil))
    let buttonRect = target.convertToScreen(button.convert(button.bounds, to: nil))
    let decoyButtonRect = decoy.convertToScreen(decoyButton.convert(decoyButton.bounds, to: nil))
    let activatingButtonRect = decoy.convertToScreen(activatingButton.convert(activatingButton.bounds, to: nil))
    let top = NSScreen.screens[0].frame.maxY
    let cursor = CGEvent(source: nil)!.location
    let frontmost = NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1
    let order = (CGWindowListCopyWindowInfo(.optionOnScreenOnly, kCGNullWindowID)
        as? [[String: Any]] ?? []).compactMap { $0[kCGWindowNumber as String] as? Int }
    everFrontmost = everFrontmost || frontmost == ProcessInfo.processInfo.processIdentifier
    if let targetIndex = order.firstIndex(of: target.windowNumber),
       let decoyIndex = order.firstIndex(of: decoy.windowNumber) {
        everRaised = everRaised || targetIndex < decoyIndex
    }
    let state: [String: Any] = [
        "pid": ProcessInfo.processInfo.processIdentifier,
        "target_id": target.windowNumber,
        "decoy_id": decoy.windowNumber,
        "x": Int(rect.midX), "y": Int(top - rect.midY),
        "button_x": Int(buttonRect.midX), "button_y": Int(top - buttonRect.midY),
        "button_presses": buttonCounter.presses,
        "decoy_button_x": Int(decoyButtonRect.midX), "decoy_button_y": Int(top - decoyButtonRect.midY),
        "decoy_button_presses": decoyButtonCounter.presses,
        "activating_button_x": Int(activatingButtonRect.midX),
        "activating_button_y": Int(top - activatingButtonRect.midY),
        "activating_button_presses": activatingButtonCounter.presses,
        "origin_x": Int(target.frame.minX), "origin_y": Int(top - target.frame.maxY),
        "target_offset": targetScroll.contentView.bounds.origin.y,
        "decoy_offset": decoyScroll.contentView.bounds.origin.y,
        "target_events": targetScroll.wheelEvents,
        "decoy_events": decoyScroll.wheelEvents,
        "received_events": receivedWheelEvents,
        "received_window": receivedWindow,
        "received_point": [receivedPoint.x, receivedPoint.y],
        "active": app.isActive,
        "activations": activations,
        "target_clicks": (targetScroll.documentView as! Document).clicks,
        "target_other_clicks": (targetScroll.documentView as! Document).otherClicks,
        "target_drags": (targetScroll.documentView as! Document).drags,
        "decoy_clicks": (decoyScroll.documentView as! Document).clicks,
        "decoy_other_clicks": (decoyScroll.documentView as! Document).otherClicks,
        "decoy_drags": (decoyScroll.documentView as! Document).drags,
        "click_counts": (targetScroll.documentView as! Document).clickCounts,
        "click_times": (targetScroll.documentView as! Document).clickTimes,
        "modifiers": (targetScroll.documentView as! Document).modifiers,
        "window_order": order,
        "ever_frontmost": everFrontmost,
        "ever_raised": everRaised,
        "frontmost_pid": frontmost,
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
    // Accessibility requests arrive over the application's main run loop,
    // not as NSEvents. Pump it without using NSApplication.run(), whose launch
    // activation would invalidate this background-only fixture.
    _ = RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.01))
    // NSApplication.run normally services window updates after each event.
    // Flush our manual pump so AX-triggered label changes reach screenshots.
    target.displayIfNeeded()
    decoy.displayIfNeeded()
    recordState()
}
