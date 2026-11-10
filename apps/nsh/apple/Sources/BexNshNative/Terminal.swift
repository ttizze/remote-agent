import Foundation
import SwiftTerm
#if os(macOS)
    import AppKit
#else
    import UIKit
#endif

public typealias TerminalCallback = @convention(c) (
    UnsafeMutableRawPointer?, Int32, UnsafePointer<UInt8>?, Int, Int32, Int32
) -> Void

private enum HostPaste {
    @TaskLocal static var active = false
}

#if os(macOS)
    struct TerminalMenu: Decodable {
        var quote = "Add to chat"
        var copy = "Copy"
        var paste = "Paste"
        var selectAll = "Select all"
        var bottom = "Jump to latest"
    }
#endif

/// Host owns terminal protocol responses. Only native typing and pasting go upstream.
final class HostTerminalView: SwiftTerm.TerminalView {
    var acceptsInput = false
    #if os(macOS)
        var menuTitles = TerminalMenu()
        var quote: ((String) -> Void)?
        private var handlingPointerEvent = false
        private var pointerChangedSelection = false
        private var pendingSelectionMenu: Timer?

        override func paste(_ sender: Any) {
            guard acceptsInput else { return }
            HostPaste.$active.withValue(true) { super.paste(sender) }
        }

        override func mouseDown(with event: NSEvent) {
            cancelSelectionMenu()
            pointerChangedSelection = false
            handlingPointerEvent = true
            defer { handlingPointerEvent = false }
            super.mouseDown(with: event)
        }

        override func mouseDragged(with event: NSEvent) {
            handlingPointerEvent = true
            defer { handlingPointerEvent = false }
            super.mouseDragged(with: event)
        }

        override nonisolated func selectionChanged(source: SwiftTerm.Terminal) {
            super.selectionChanged(source: source)
            if Thread.isMainThread {
                MainActor.assumeIsolated {
                    if handlingPointerEvent {
                        pointerChangedSelection = true
                    }
                }
            }
        }

        override func mouseUp(with event: NSEvent) {
            handlingPointerEvent = true
            super.mouseUp(with: event)
            handlingPointerEvent = false
            guard pointerChangedSelection, let selected = menuSelection else { return }
            let point = convert(event.locationInWindow, from: nil)
            // Leave the next click free to extend a word selection to its line.
            let delay = event.clickCount >= 2 ? NSEvent.doubleClickInterval : 0
            let request = Timer(timeInterval: delay, repeats: false) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    pendingSelectionMenu = nil
                    guard window?.firstResponder === self, !isHiddenOrHasHiddenAncestor,
                          menuSelection == selected else { return }
                    selectionMenu().popUp(positioning: nil, at: point, in: self)
                }
            }
            pendingSelectionMenu = request
            // A run-loop timer leaves the main dispatch queue free while
            // AppKit runs the menu's nested event loop.
            RunLoop.main.add(request, forMode: .default)
        }

        func cancelSelectionMenu() {
            pendingSelectionMenu?.invalidate()
            pendingSelectionMenu = nil
        }

        override func menu(for _: NSEvent) -> NSMenu? {
            cancelSelectionMenu()
            let menu = selectionMenu()
            menu.addItem(item(menuTitles.paste, #selector(paste(_:)), enabled: acceptsInput))
            menu.addItem(item(menuTitles.selectAll, #selector(selectAll(_:))))
            menu.addItem(item(menuTitles.bottom, #selector(jumpToLatest(_:))))
            return menu
        }

        /// Snapshot the text while the menu is open; a changed selection cannot
        /// add different output to the conversation.
        func selectionMenu() -> NSMenu {
            let menu = NSMenu()
            menu.autoenablesItems = false
            let selected = menuSelection
            let add = item(menuTitles.quote, #selector(quoteSelection(_:)), enabled: selected != nil)
            add.representedObject = selected
            menu.addItem(add)
            let copy = item(menuTitles.copy, #selector(copySelection(_:)), enabled: selected != nil)
            copy.representedObject = selected
            menu.addItem(copy)
            return menu
        }

        private var menuSelection: String? {
            getSelection().flatMap { $0.trimmingCharacters(in: .newlines).isEmpty ? nil : $0 }
        }

        private func item(_ title: String, _ action: Selector, enabled: Bool = true) -> NSMenuItem {
            let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
            item.target = self
            item.isEnabled = enabled
            return item
        }

        @objc private func quoteSelection(_ sender: NSMenuItem) {
            guard let selected = sender.representedObject as? String, getSelection() == selected else { return }
            quote?(selected.replacingOccurrences(of: "\r\n", with: "\n").trimmingCharacters(in: .newlines))
            selectNone()
        }

        @objc private func copySelection(_ sender: NSMenuItem) {
            guard let selected = sender.representedObject as? String else { return }
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(selected, forType: .string)
        }

        @objc private func jumpToLatest(_: Any?) {
            scroll(toPosition: 1)
        }
    #else
        override func paste(_ sender: Any?) {
            guard acceptsInput else { return }
            HostPaste.$active.withValue(true) { super.paste(sender) }
        }
    #endif

    override nonisolated func send(source: SwiftTerm.Terminal, data: ArraySlice<UInt8>) {
        guard HostPaste.active else { return }
        super.send(source: source, data: data)
    }

    #if os(macOS)
        /// SwiftTerm's iOS view reads its screen to VoiceOver; its Mac view has no
        /// accessibility text. The visible rows are this text area's value, which
        /// SwiftTerm already announces as changed after each frame it draws.
        override func isAccessibilityElement() -> Bool {
            true
        }

        override func accessibilityRole() -> NSAccessibility.Role? {
            .textArea
        }

        override func accessibilityValue() -> Any? {
            screenText()
        }

        private var drawn: SwiftTerm.Terminal?

        /// The Mac view keeps the `Terminal` it draws internal; reflection
        /// reaches it once SwiftTerm has created it.
        private func drawnTerminal() -> SwiftTerm.Terminal? {
            var mirror: Mirror? = Mirror(reflecting: self)
            while drawn == nil, let current = mirror {
                drawn = current.children.first { $0.label == "terminal" }?.value as? SwiftTerm.Terminal
                mirror = current.superclassMirror
            }
            return drawn
        }

        func screenText() -> String {
            guard let terminal = drawnTerminal() else { return "" }
            let rows = terminal.terminalLock.withLock {
                (0 ..< terminal.rows).map { terminal.getLine(row: $0)?.translateToString(
                    trimRight: true,
                    skipNullCellsFollowingWide: true
                ) ?? "" }
            }
            return rows.reversed().drop(while: \.isEmpty).reversed().joined(separator: "\n")
        }
    #endif
}

@MainActor private final class TerminalOwner: NSObject, TerminalViewDelegate {
    let view = HostTerminalView(frame: .zero)
    let context: UnsafeMutableRawPointer?
    var callback: TerminalCallback?
    var restoring = false
    #if os(macOS)
        var windowUpdates: NSObjectProtocol?
        var focused = false

        /// Whether the terminal holds the keyboard, for shortcuts that skip it.
        func reportFocus() {
            let now = view.window?.firstResponder === view
            guard now != focused else { return }
            focused = now
            callback?(context, 3, nil, 0, now ? 1 : 0, 0)
        }

        func stopReportingFocus() {
            if let windowUpdates {
                NotificationCenter.default.removeObserver(windowUpdates)
            }
            windowUpdates = nil
        }
    #endif

    init(context: UnsafeMutableRawPointer?, callback: @escaping TerminalCallback) {
        self.context = context
        self.callback = callback
        super.init()
        view.terminalDelegate = self
        #if os(macOS)
            view.quote = { [weak self] text in
                guard let self else { return }
                let bytes = Array(text.utf8)
                bytes.withUnsafeBufferPointer { buffer in
                    self.callback?(self.context, 4, buffer.baseAddress, buffer.count, 0, 0)
                }
            }
            // SwiftTerm keeps its responder methods closed, so the window's
            // updates tell when the terminal takes or gives up the keyboard.
            windowUpdates = NotificationCenter.default.addObserver(
                forName: NSWindow.didUpdateNotification, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.reportFocus() }
            }
        #endif
        view.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        #if os(macOS)
            // The Mac window is drawn dark by the app, not by the system appearance.
            view.nativeBackgroundColor = NSColor(srgbRed: 0x08 / 255, green: 0x08 / 255, blue: 0x08 / 255, alpha: 1)
            view.nativeForegroundColor = NSColor(srgbRed: 0xE6 / 255, green: 0xE6 / 255, blue: 0xE6 / 255, alpha: 1)
        #endif
        #if os(iOS)
            view.accessibilityIdentifier = "terminal.screen"
        #endif
        // Report the native size after NSH has installed the view and its event handlers.
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            let terminal = view.terminalDimensions
            sizeChanged(source: view, newCols: terminal.cols, newRows: terminal.rows)
        }
    }

    func send(source _: SwiftTerm.TerminalView, data: ArraySlice<UInt8>) {
        guard view.acceptsInput else { return }
        data.withUnsafeBufferPointer { buffer in
            callback?(context, 1, buffer.baseAddress, buffer.count, 0, 0)
        }
    }

    func sizeChanged(source _: SwiftTerm.TerminalView, newCols: Int, newRows: Int) {
        guard !restoring, newCols > 0, newRows > 0 else { return }
        callback?(context, 2, nil, 0, Int32(clamping: newCols), Int32(clamping: newRows))
    }

    func setTerminalTitle(source _: SwiftTerm.TerminalView, title _: String) {}
    func hostCurrentDirectoryUpdate(source _: SwiftTerm.TerminalView, directory _: String?) {}
    func scrolled(source _: SwiftTerm.TerminalView, position _: Double) {}
    func rangeChanged(source _: SwiftTerm.TerminalView, startY _: Int, endY _: Int) {}

    func requestOpenLink(source _: SwiftTerm.TerminalView, link: String, params _: [String: String]) {
        guard let url = URL(string: link), ["https", "http"].contains(url.scheme?.lowercased() ?? "") else { return }
        #if os(macOS)
            NSWorkspace.shared.open(url)
        #else
            UIApplication.shared.open(url)
        #endif
    }
}

/// NSH's native component calls these on the main thread and owns the retained handle.
@_cdecl("bex_nsh_terminal_create")
@MainActor public func terminalCreate(_ context: UnsafeMutableRawPointer?,
                                      _ callback: @escaping TerminalCallback) -> UnsafeMutableRawPointer {
    Unmanaged.passRetained(TerminalOwner(context: context, callback: callback)).toOpaque()
}

@_cdecl("bex_nsh_terminal_view")
@MainActor public func terminalView(_ handle: UnsafeMutableRawPointer) -> UnsafeMutableRawPointer {
    Unmanaged.passUnretained(Unmanaged<TerminalOwner>.fromOpaque(handle).takeUnretainedValue().view).toOpaque()
}

@_cdecl("bex_nsh_terminal_input")
@MainActor public func terminalInput(_ handle: UnsafeMutableRawPointer, _ enabled: Bool) {
    Unmanaged<TerminalOwner>.fromOpaque(handle).takeUnretainedValue().view.acceptsInput = enabled
}

#if os(macOS)
    @_cdecl("bex_nsh_terminal_menu")
    @MainActor public func terminalMenu(
        _ handle: UnsafeMutableRawPointer,
        _ bytes: UnsafePointer<UInt8>?,
        _ count: Int
    ) {
        guard let bytes, count > 0,
              let titles = try? JSONDecoder().decode(TerminalMenu.self, from: Data(bytes: bytes, count: count))
        else { return }
        Unmanaged<TerminalOwner>.fromOpaque(handle).takeUnretainedValue().view.menuTitles = titles
    }
#endif

@_cdecl("bex_nsh_terminal_feed")
@MainActor public func terminalFeed(
    _ handle: UnsafeMutableRawPointer,
    _ bytes: UnsafePointer<UInt8>?,
    _ count: Int,
    _ columns: Int32,
    _ rows: Int32
) {
    let owner = Unmanaged<TerminalOwner>.fromOpaque(handle).takeUnretainedValue()
    owner.restoring = true
    defer { owner.restoring = false }
    if columns > 0, rows > 0 {
        owner.view.resize(cols: Int(columns), rows: Int(rows))
    }
    if let bytes, count > 0 {
        owner.view.feed(byteArray: Array(UnsafeBufferPointer(start: bytes, count: count))[...])
    }
}

@_cdecl("bex_nsh_terminal_release")
@MainActor public func terminalRelease(_ handle: UnsafeMutableRawPointer) {
    let owner = Unmanaged<TerminalOwner>.fromOpaque(handle).takeRetainedValue()
    owner.view.terminalDelegate = nil
    #if os(macOS)
        owner.view.cancelSelectionMenu()
        owner.stopReportingFocus()
    #endif
    owner.callback = nil
}
