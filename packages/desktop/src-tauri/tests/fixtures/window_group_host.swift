// Retained, titleless dialog and passive companion with identical bounds.
// Commands change only these isolated fixture windows, without activation.
import AppKit

let stateURL = URL(fileURLWithPath: CommandLine.arguments[1])
let commandURL = stateURL.deletingPathExtension().appendingPathExtension("command")
let app = NSApplication.shared
app.setActivationPolicy(.regular)
app.finishLaunching()
app.deactivate()

final class FixtureWindow: NSWindow {
    var axSubrole: NSAccessibility.Subrole = .standardWindow
    weak var axOwner: NSWindow?
    var passive = false
    override func accessibilitySubrole() -> NSAccessibility.Subrole? { axSubrole }
    override func accessibilityParent() -> Any? { axOwner ?? super.accessibilityParent() }
    override func accessibilityRole() -> NSAccessibility.Role? { passive ? .image : .window }
    override func accessibilityWindow() -> Any? { self }
    override func isAccessibilityEnabled() -> Bool { !passive }
}

final class Surface: NSView {
    var kind = "root"
    override var isFlipped: Bool { true }
    override func draw(_ rect: NSRect) {
        switch kind {
        case "dialog":
            NSColor(srgbRed: 1, green: 1, blue: 1, alpha: 0.5).setFill()
            bounds.fill()
            NSColor.white.setFill()
            NSRect(x: 140, y: 80, width: 80, height: 80).fill()
        case "companion":
            NSColor.white.setFill()
            NSRect(x: 10, y: 10, width: 20, height: 20).fill()
        default:
            NSColor.black.setFill()
            bounds.fill()
        }
    }
}

func makeWindow(_ kind: String, _ subrole: NSAccessibility.Subrole) -> FixtureWindow {
    let window = FixtureWindow(contentRect: NSRect(x: 240, y: 200, width: 360, height: 240),
                               styleMask: .borderless, backing: .buffered, defer: false)
    window.axSubrole = subrole
    window.title = ""
    window.isReleasedWhenClosed = false
    window.isOpaque = false
    window.hasShadow = false
    window.backgroundColor = .clear
    let surface = Surface(frame: window.contentView!.bounds)
    surface.kind = kind
    window.contentView = surface
    window.display()
    return window
}

let occluder = CommandLine.arguments.count > 2
let root = makeWindow(occluder ? "occluder" : "root", .standardWindow)
let dialog = makeWindow("dialog", .dialog)
let companion = makeWindow("companion", .unknown)
// These fixtures expose explicit ownership/passivity. Matching bounds or
// addChildWindow alone must never be the proof used by the host.
dialog.axOwner = root
companion.axOwner = root
companion.passive = true
if occluder {
    root.order(.above, relativeTo: Int(CommandLine.arguments[2])!)
} else {
    root.orderBack(nil)
    root.addChildWindow(dialog, ordered: .above)
    root.addChildWindow(companion, ordered: .above)
}
var phase = "open"
var readyAfter = Date(timeIntervalSinceNow: 0.3)

while true {
    if let event = app.nextEvent(matching: .any, until: Date(timeIntervalSinceNow: 0.02),
                                 inMode: .default, dequeue: true) {
        app.sendEvent(event)
    }
    _ = RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.01))
    if let command = try? String(contentsOf: commandURL, encoding: .utf8), command != phase {
        if command == "closed" {
            // Keep the NSWindow (and potentially its server backing) alive.
            dialog.orderOut(nil)
        } else if command == "reopened" {
            dialog.order(.above, relativeTo: root.windowNumber)
            companion.order(.above, relativeTo: dialog.windowNumber)
        }
        phase = command
        readyAfter = Date(timeIntervalSinceNow: 0.3)
    }
    if Date() < readyAfter { continue }
    let state: [String: Any] = [
        "pid": ProcessInfo.processInfo.processIdentifier,
        "root_id": root.windowNumber, "dialog_id": dialog.windowNumber,
        "companion_id": companion.windowNumber, "phase": phase,
        "active": app.isActive,
    ]
    try! JSONSerialization.data(withJSONObject: state).write(to: stateURL, options: .atomic)
}
