// An independently ordered popup exposes its role but no AX owner, as WeChat
// search does. An overlapping sibling document must never become its substitute.
import AppKit

let stateURL = URL(fileURLWithPath: CommandLine.arguments[1])
let app = NSApplication.shared
app.setActivationPolicy(.regular)
app.finishLaunching()
final class Popup: NSPanel {
    override func accessibilityRole() -> NSAccessibility.Role? { .window }
    override func accessibilitySubrole() -> NSAccessibility.Subrole? { .floatingWindow }
    override func accessibilityParent() -> Any? { NSApplication.shared }
    override func accessibilityWindow() -> Any? { self }
}
func document(_ title: String) -> NSWindow {
    let window = NSWindow(contentRect: NSRect(x: 240, y: 240, width: 440, height: 320),
                          styleMask: [.titled, .closable], backing: .buffered, defer: false)
    window.title = title
    window.isReleasedWhenClosed = false
    let text = NSTextField(labelWithString: title)
    text.frame = NSRect(x: 20, y: 270, width: 360, height: 24)
    window.contentView!.addSubview(text)
    return window
}
let root = document("Root content")
let sibling = document("Sibling document")
root.orderBack(nil)
sibling.order(.above, relativeTo: root.windowNumber)
let popup = Popup(contentRect: NSRect(x: 280, y: 310, width: 260, height: 140),
                  styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
popup.isReleasedWhenClosed = false
popup.hidesOnDeactivate = false
popup.level = .normal
popup.title = "Independent search results"
var clicks = 0
final class Handler: NSObject {
    @objc func choose(_ sender: NSButton) {
        clicks += 1
        popup.orderOut(nil)
    }
}
let handler = Handler()
let button = NSButton(title: "Fixture search result", target: handler, action: #selector(Handler.choose(_:)))
button.frame = NSRect(x: 20, y: 40, width: 220, height: 44)
popup.contentView!.addSubview(button)
popup.order(.above, relativeTo: sibling.windowNumber)
let rect = popup.convertToScreen(button.frame)
let ready = Date(timeIntervalSinceNow: 0.4)
while true {
    if let event = app.nextEvent(matching: .any, until: Date(timeIntervalSinceNow: 0.01), inMode: .default, dequeue: true) {
        app.sendEvent(event)
    }
    _ = RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.01))
    if Date() < ready { continue }
    let state: [String: Any] = ["pid": ProcessInfo.processInfo.processIdentifier,
        "root_id": root.windowNumber, "popup_id": popup.windowNumber, "sibling_id": sibling.windowNumber,
        "clicks": clicks, "button_x": rect.midX, "button_y": NSScreen.screens[0].frame.maxY - rect.midY]
    try! JSONSerialization.data(withJSONObject: state).write(to: stateURL, options: .atomic)
}
