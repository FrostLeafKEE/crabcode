// A real NSSavePanel sheet, isolated in a temporary application and directory.
import AppKit

let stateURL = URL(fileURLWithPath: CommandLine.arguments[1])
// Publish the PID before a native panel/XPC startup can block, so the test
// harness can always terminate its own fixture on failure.
try! JSONSerialization.data(withJSONObject: [
    "pid": ProcessInfo.processInfo.processIdentifier, "phase": "starting"
]).write(to: stateURL, options: .atomic)
let app = NSApplication.shared
app.setActivationPolicy(.regular)
app.finishLaunching()
let root = NSWindow(contentRect: NSRect(x: 280, y: 260, width: 680, height: 520),
                    styleMask: [.titled, .closable], backing: .buffered, defer: false)
root.title = "Save fixture document"
root.isReleasedWhenClosed = false
let sibling = NSWindow(contentRect: NSRect(x: 1100, y: 260, width: 360, height: 240),
                       styleMask: [.titled, .closable], backing: .buffered, defer: false)
sibling.title = "Unrelated fixture document"
sibling.isReleasedWhenClosed = false
sibling.orderBack(nil)
root.makeKeyAndOrderFront(nil)
app.activate(ignoringOtherApps: true)
let panel = NSSavePanel()
panel.title = "Save fixture"
panel.prompt = "Save fixture"
panel.nameFieldStringValue = "report.txt"
panel.directoryURL = stateURL.deletingLastPathComponent()
panel.canCreateDirectories = true
var completed = false
var savedPath = ""
panel.beginSheetModal(for: root) { response in
    if response == .OK, let url = panel.url {
        // Only this temporary fixture writes the test file.
        try! "line 1\nline 2\nline 3\n".write(to: url, atomically: true, encoding: .utf8)
        savedPath = url.path
    }
    completed = true
}
while true {
    if let event = app.nextEvent(matching: .any, until: Date(timeIntervalSinceNow: 0.02),
                                 inMode: .default, dequeue: true) { app.sendEvent(event) }
    _ = RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.01))
    let state: [String: Any] = [
        "pid": ProcessInfo.processInfo.processIdentifier,
        "root": root.windowNumber, "sibling": sibling.windowNumber,
        "panel": panel.windowNumber, "completed": completed,
        "saved_path": savedPath, "phase": "ready",
    ]
    try! JSONSerialization.data(withJSONObject: state).write(to: stateURL, options: .atomic)
}
