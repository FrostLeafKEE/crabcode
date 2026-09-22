// Isolated keyboard regression host. Never reads or writes the system clipboard.
import AppKit

let stateURL = URL(fileURLWithPath: CommandLine.arguments[1])
let commandURL = stateURL.deletingPathExtension().appendingPathExtension("command")
let app = NSApplication.shared
app.setActivationPolicy(.regular)
var copies = 0
final class Editor: NSTextView {
    override func copy(_ sender: Any?) { copies += 1 }
}
func makeWindow(_ title: String, _ x: CGFloat) -> (NSWindow, Editor) {
    let window = NSWindow(contentRect: NSRect(x: x, y: 200, width: 360, height: 240),
                          styleMask: [.titled, .closable], backing: .buffered, defer: false)
    window.title = title
    window.isReleasedWhenClosed = false
    let editor = Editor(frame: window.contentView!.bounds)
    editor.string = title
    window.contentView = editor
    window.makeFirstResponder(editor)
    window.orderBack(nil)
    return (window, editor)
}
let (first, firstEditor) = makeWindow("first", 200)
let (second, secondEditor) = makeWindow("second", 600)
let panel = NSPanel(contentRect: NSRect(x: 260, y: 280, width: 280, height: 80),
                    styleMask: [.titled], backing: .buffered, defer: false)
panel.title = "Name"
panel.isReleasedWhenClosed = false
let name = Editor(frame: panel.contentView!.bounds)
name.string = "untitled"
panel.contentView = name
let menu = NSMenu()
let edit = NSMenuItem()
edit.submenu = NSMenu(title: "Edit")
edit.submenu!.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
edit.submenu!.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
menu.addItem(edit)
app.mainMenu = menu
app.finishLaunching()
var phase = "ready"
while true {
    if let event = app.nextEvent(matching: .any, until: Date(timeIntervalSinceNow: 0.01), inMode: .default, dequeue: true) {
        app.sendEvent(event)
    }
    _ = RunLoop.current.run(mode: .default, before: Date(timeIntervalSinceNow: 0.01))
    if let command = try? String(contentsOf: commandURL, encoding: .utf8), command != phase {
        if command == "panel" {
            first.addChildWindow(panel, ordered: .above)
            panel.makeKeyAndOrderFront(nil)
            panel.makeFirstResponder(name)
            name.selectAll(nil)
        }
        phase = command
    }
    let state: [String: Any] = [
        "pid": ProcessInfo.processInfo.processIdentifier,
        "first": first.windowNumber, "second": second.windowNumber, "panel": panel.windowNumber,
        "first_text": firstEditor.string, "second_text": secondEditor.string,
        "name": name.string, "copies": copies, "phase": phase,
        "key": app.keyWindow?.windowNumber ?? 0,
    ]
    try! JSONSerialization.data(withJSONObject: state).write(to: stateURL, options: .atomic)
}
