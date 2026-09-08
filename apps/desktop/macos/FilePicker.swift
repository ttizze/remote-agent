import AppKit

/// A native panel process keeps AppKit's modal loop off the GPUI event loop.
/// Only the selected path crosses stdout; cancel is JSON null.
let application = NSApplication.shared
application.setActivationPolicy(.regular)
application.finishLaunching()
application.activate(ignoringOtherApps: true)
let mode = CommandLine.arguments.dropFirst().first ?? "file"
let panel: NSSavePanel
switch mode {
case "file", "folder":
    let open = NSOpenPanel()
    open.canChooseFiles = mode == "file"
    open.canChooseDirectories = mode == "folder"
    open.allowsMultipleSelection = false
    open.message = mode == "file" ? "添付するファイルを選択" : "作業フォルダを選択"
    panel = open
case let value where value == "download" || value.hasPrefix("download:"):
    panel = NSSavePanel()
    panel.nameFieldStringValue = mode.hasPrefix("download:") ? String(mode.dropFirst("download:".count)) : "download"
    panel.message = "保存先を選択"
default:
    exit(2)
}
panel.canCreateDirectories = true
let selected = panel.runModal() == .OK ? panel.url?.path : nil
try FileHandle.standardOutput.write(JSONEncoder().encode(selected))
