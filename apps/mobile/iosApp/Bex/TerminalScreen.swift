import AgentCore
import SwiftTerm
import SwiftUI

/// One of a thread's terminals, by (thread, terminal id). A `nil` id opens a
/// new terminal once the view knows its size.
struct TerminalScreen: View {
    @ObservedObject var model: BexAppViewModel
    let threadId: String
    @State var terminalId: String?
    /// Changes when the user picks another terminal, replacing the view.
    @State private var session = UUID()
    @StateObject private var keys = TerminalKeys()
    @AppStorage("terminal.fontSize") private var fontSize = 10.5

    var body: some View {
        VStack(spacing: 0) {
            if let terminalId {
                let terminal = model.snapshot.terminal(threadId: threadId, terminalId: terminalId, after: UInt64.max)
                if terminal.loading {
                    ProgressView("Opening terminal…").accessibilityIdentifier("terminal.loading").padding(8)
                } else if let status = terminal.status {
                    Text(status).font(AppTheme.font(12)).foregroundStyle(AppTheme.muted).padding(4)
                }
            }
            NativeTerminalView(model: model, threadId: threadId, terminal: terminalId,
                               opened: { terminalId = $0 }, keys: keys, fontSize: fontSize)
                .id(session)
            KeyBar(keys: keys)
        }
        .background(AppTheme.color("terminalBackground").ignoresSafeArea())
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 1) {
                    Text("Terminal").font(AppTheme.font(17, weight: .heavy))
                    if let project = model.threadView?.header?.project?.name {
                        Text(project).font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
                    }
                }
            }
            ToolbarItem(placement: .topBarTrailing) { options }
        }
    }

    private var options: some View {
        Menu {
            Section("Text size") {
                Button("A- \(String(format: "%.1f", fontSize - 0.5)) pt") { fontSize = max(6, fontSize - 0.5) }
                Button("A+ \(String(format: "%.1f", fontSize + 0.5)) pt") { fontSize = min(14, fontSize + 0.5) }
            }
            ForEach(model.snapshot.terminals(threadId: threadId), id: \.terminalId) { tab in
                Button { switchTo(tab.terminalId) } label: {
                    if tab.terminalId == terminalId {
                        Label(tab.label, systemImage: "checkmark")
                    } else {
                        Label(tab.label, systemImage: "terminal")
                    }
                    Text(tab.status)
                }
            }
            Button { switchTo(nil) } label: {
                Label("Open new terminal", systemImage: "plus")
                Text("Start another shell for this thread")
            }
            if let terminalId {
                Button("Close terminal", role: .destructive) {
                    model.perform(.closeTerminal(threadId: threadId, terminalId: terminalId))
                    switchTo(nil)
                }
            }
        } label: {
            Image(systemName: "terminal")
        }
        .accessibilityLabel("Terminal options")
    }

    private func switchTo(_ terminal: String?) {
        terminalId = terminal
        session = UUID()
    }
}

/// Sticky modifiers and keys the software keyboard lacks.
@MainActor
final class TerminalKeys: ObservableObject {
    @Published var control = false
    @Published var alt = false
    var send: ((Data) -> Void)?
    var paste: (() -> Void)?

    func transform(_ bytes: [UInt8]) -> [UInt8] {
        var output = bytes
        if control, let first = output.first {
            output[0] = first & 0x1F
            control = false
        }
        if alt {
            output.insert(0x1B, at: 0)
            alt = false
        }
        return output
    }
}

private struct KeyBar: View {
    @ObservedObject var keys: TerminalKeys

    var body: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 6) {
                key("esc", [0x1B])
                toggle("CTRL", $keys.control)
                toggle("ALT", $keys.alt)
                key("tab", [0x09])
                Button("paste") { keys.paste?() }.buttonStyle(KeyStyle(active: false))
                key("CLEAR", Array("clear\r".utf8))
                key("↑", [0x1B, 0x5B, 0x41])
                key("↓", [0x1B, 0x5B, 0x42])
                key("←", [0x1B, 0x5B, 0x44])
                key("→", [0x1B, 0x5B, 0x43])
                key("~", Array("~".utf8))
                key("|", Array("|".utf8))
                key("/", Array("/".utf8))
                key("-", Array("-".utf8))
                Button {
                    UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil,
                                                    for: nil)
                } label: { Image(systemName: "keyboard.chevron.compact.down") }
                    .buttonStyle(KeyStyle(active: false))
                    .accessibilityLabel("Dismiss keyboard")
            }
            .padding(.horizontal, 8).padding(.vertical, 4)
        }
        .scrollIndicators(.hidden)
        .frame(minHeight: 52)
        .overlay(alignment: .top) { Rectangle().fill(AppTheme.border).frame(height: 1) }
    }

    private func key(_ label: String, _ bytes: [UInt8]) -> some View {
        Button(label) { keys.send?(Data(keys.transform(bytes))) }
            .buttonStyle(KeyStyle(active: false))
    }

    private func toggle(_ label: String, _ value: Binding<Bool>) -> some View {
        Button(label) { value.wrappedValue.toggle() }
            .buttonStyle(KeyStyle(active: value.wrappedValue))
    }
}

private struct KeyStyle: ButtonStyle {
    let active: Bool

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(AppTheme.font(14, weight: .bold))
            .foregroundStyle(AppTheme.color("terminalForeground"))
            .padding(.horizontal, 12)
            .frame(minWidth: 44, minHeight: 38.5)
            .background(active || configuration.isPressed ? AppTheme.subtleStrong : AppTheme.subtle, in: Capsule())
            .overlay(Capsule().stroke(AppTheme.border))
    }
}

private struct NativeTerminalView: UIViewRepresentable {
    @ObservedObject var model: BexAppViewModel
    let threadId: String
    let terminal: String?
    let opened: (String) -> Void
    let keys: TerminalKeys
    let fontSize: Double

    func makeCoordinator() -> Coordinator {
        Coordinator(model: model, threadId: threadId, terminal: terminal, opened: opened, keys: keys)
    }

    func makeUIView(context: Context) -> SwiftTerm.TerminalView {
        let view = HostTerminalView(frame: .zero)
        view.terminalDelegate = context.coordinator
        view.font = .monospacedSystemFont(ofSize: fontSize, weight: .regular)
        view.nativeBackgroundColor = AppTheme.uiColor("terminalBackground")
        view.nativeForegroundColor = AppTheme.uiColor("terminalForeground")
        view.caretColor = AppTheme.uiColor("terminalCursor")
        view.inputAccessoryView = nil
        view.accessibilityIdentifier = "terminal.screen"
        keys.paste = { [weak view] in view?.paste(nil) }
        return view
    }

    static func dismantleUIView(_ view: SwiftTerm.TerminalView, coordinator: Coordinator) {
        view.terminalDelegate = nil
        coordinator.dismantled = true
        if coordinator.started, let terminal = coordinator.terminal {
            coordinator.model.perform(.detachTerminal(threadId: coordinator.threadId, terminalId: terminal))
        }
    }

    func updateUIView(_ view: SwiftTerm.TerminalView, context: Context) {
        let coordinator = context.coordinator
        if view.font.pointSize != fontSize {
            view.font = .monospacedSystemFont(ofSize: fontSize, weight: .regular)
        }
        keys.send = { coordinator.write($0) }
        guard let terminal = coordinator.terminal else { return }
        let state = model.snapshot.terminal(threadId: threadId, terminalId: terminal, after: coordinator.sequence)
        coordinator.acceptsInput = state.acceptsInput
        view.accessibilityValue = state.acceptsInput && !state.loading ? "Ready" : nil
        for chunk in state.output where chunk.sequence > coordinator.sequence {
            if let size = chunk.resetSize {
                coordinator.restoring = true
                view.resize(cols: Int(size.cols), rows: Int(size.rows))
            }
            view.feed(byteArray: Array(chunk.data)[...])
            coordinator.restoring = false
            coordinator.sequence = chunk.sequence
        }
    }

    @MainActor final class Coordinator: NSObject, TerminalViewDelegate {
        let model: BexAppViewModel
        let threadId: String
        var terminal: String?
        let opened: (String) -> Void
        let keys: TerminalKeys
        var sequence: UInt64 = 0
        var acceptsInput = false
        var restoring = false
        var started = false
        var dismantled = false

        init(model: BexAppViewModel, threadId: String, terminal: String?, opened: @escaping (String) -> Void,
             keys: TerminalKeys) {
            self.model = model
            self.threadId = threadId
            self.terminal = terminal
            self.opened = opened
            self.keys = keys
        }

        func write(_ data: Data) {
            guard !dismantled, acceptsInput, let terminal else { return }
            model.perform(.writeTerminal(threadId: threadId, terminalId: terminal, data: data))
        }

        func send(source _: SwiftTerm.TerminalView, data: ArraySlice<UInt8>) {
            write(Data(keys.transform(Array(data))))
        }

        func sizeChanged(source _: SwiftTerm.TerminalView, newCols: Int, newRows: Int) {
            guard !dismantled, !restoring, newCols > 0, newRows > 0 else { return }
            let cols = UInt16(clamping: min(newCols, 500))
            let rows = UInt16(clamping: min(newRows, 250))
            Task { @MainActor in
                guard !dismantled else { return }
                if !started {
                    started = true
                    open(cols: cols, rows: rows)
                } else if acceptsInput, let terminal {
                    model.perform(.resizeTerminal(threadId: threadId, terminalId: terminal, cols: cols, rows: rows))
                }
            }
        }

        private func open(cols: UInt16, rows: UInt16) {
            if let terminal {
                model.perform(.openTerminal(threadId: threadId, terminalId: terminal, cols: cols, rows: rows))
                return
            }
            model.perform(.newTerminal(threadId: threadId, cols: cols, rows: rows)) { [weak self] result in
                guard let self, !dismantled, case let .success(.terminalOpened(terminal)) = result else { return }
                self.terminal = terminal
                opened(terminal)
            }
        }

        func setTerminalTitle(source _: SwiftTerm.TerminalView, title _: String) {}
        func hostCurrentDirectoryUpdate(source _: SwiftTerm.TerminalView, directory _: String?) {}
        func scrolled(source _: SwiftTerm.TerminalView, position _: Double) {}
        func rangeChanged(source _: SwiftTerm.TerminalView, startY _: Int, endY _: Int) {}
        func requestOpenLink(source _: SwiftTerm.TerminalView, link: String, params _: [String: String]) {
            guard let url = URL(string: link),
                  ["https", "http"].contains(url.scheme?.lowercased() ?? "") else { return }
            UIApplication.shared.open(url)
        }
    }
}

/// Emulator responses are produced by the Host; SwiftTerm still handles native user input.
private final class HostTerminalView: SwiftTerm.TerminalView {
    override func paste(_ sender: Any?) {
        // SwiftTerm sends paste bytes synchronously through the emulator delegate.
        // Scope forwarding to this call, without enabling replies from its parser thread.
        TerminalPaste.$isPasting.withValue(true) { super.paste(sender) }
    }

    override nonisolated func send(source: SwiftTerm.Terminal, data: ArraySlice<UInt8>) {
        guard TerminalPaste.isPasting else { return }
        super.send(source: source, data: data)
    }
}

private enum TerminalPaste { @TaskLocal static var isPasting = false }
