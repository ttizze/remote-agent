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
    /// The terminal last seen running here, so its exit can leave it.
    @State private var runningTerminal: String?
    /// The visible lines captured for the attach sheet.
    @State private var captured: [String]?
    @State private var noOutput = false
    @State private var attachError: String?
    @Environment(\.dismiss) private var dismiss

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
            if terminalId != nil {
                Button(action: capture) {
                    Text("Attach visible output").font(AppTheme.font(16))
                        .foregroundStyle(AppTheme.color("terminalForeground"))
                        .padding(.horizontal, 16).padding(.vertical, 8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .buttonStyle(.plain)
            }
            KeyBar(keys: keys) {
                if let terminalId {
                    model.perform(.clearTerminal(threadId: threadId, terminalId: terminalId))
                }
            }
        }
        .onChange(of: currentTab) { _, tab in followExit(tab) }
        .sheet(isPresented: Binding(get: { captured != nil }, set: {
            if !$0 {
                captured = nil
            }
        })) {
            if let captured, let terminalId {
                TerminalContextSheet(lines: captured, terminalId: terminalId,
                                     terminalLabel: currentTab?.label ?? "Terminal", attach: attach)
            }
        }
        .alert("No terminal output", isPresented: $noOutput) {} message: {
            Text("There is no visible output to attach.")
        }
        .alert("Too many context items", isPresented: Binding(
            get: { attachError != nil }, set: {
                if !$0 {
                    attachError = nil
                }
            }
        )) {} message: {
            Text("Remove some context from the draft and try again.")
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

    private var tabs: [TerminalTab] {
        model.snapshot.terminals(threadId: threadId)
    }

    private var currentTab: TerminalTab? {
        tabs.first { $0.terminalId == terminalId }
    }

    private var options: some View {
        Menu {
            Section("Text size") {
                Button("A- \(String(format: "%.1f", stepped(-1))) pt") { fontSize = stepped(-1) }
                    .disabled(fontSize <= 6)
                Button("A+ \(String(format: "%.1f", stepped(1))) pt") { fontSize = stepped(1) }
                    .disabled(fontSize >= 14)
            }
            ForEach(tabs.filter { $0.running || $0.terminalId == terminalId }, id: \.terminalId) { tab in
                Button { switchTo(tab.terminalId) } label: {
                    if tab.terminalId == terminalId {
                        Label(tab.label, systemImage: "checkmark")
                    } else {
                        Label(tab.label, systemImage: "terminal")
                    }
                    Text(tab.menuSubtitle)
                }
            }
            Button { switchTo(nil) } label: {
                Label("Open new terminal", systemImage: "plus")
                Text("Start another shell in \(workspaceName)")
            }
        } label: {
            Image(systemName: "terminal")
        }
        .accessibilityLabel("Terminal options")
    }

    /// Freezes the visible output for the attach sheet.
    private func capture() {
        UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
        let text = keys.capture?() ?? ""
        if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            noOutput = true
        } else {
            captured = visibleOutputLines(text: text)
        }
    }

    /// Adds the chosen lines to the thread's draft and returns to the thread.
    private func attach(_ output: TerminalOutputContext) {
        model.perform(.attachTerminalOutput(output: output)) { result in
            captured = nil
            switch result {
            case .success: dismiss()
            case let .failure(error): attachError = error.localizedDescription
            }
        }
    }

    /// The text size one 0.5 pt step away, within 6–14 pt.
    private func stepped(_ direction: Double) -> Double {
        min(14, max(6, fontSize + direction * 0.5))
    }

    private var workspaceName: String {
        let name = URL(fileURLWithPath: model.cwd).lastPathComponent
        return model.cwd.isEmpty || name.isEmpty ? "this workspace" : name
    }

    private func switchTo(_ terminal: String?) {
        terminalId = terminal
        runningTerminal = nil
        session = UUID()
    }

    /// A shell that ends here is closed, and the screen moves to the nearest
    /// live terminal before it, else after it, else back to the thread.
    private func followExit(_ tab: TerminalTab?) {
        guard let terminalId else { return }
        if let tab, tab.running {
            runningTerminal = terminalId
            return
        }
        guard runningTerminal == terminalId, tab == nil || tab?.exited == true else { return }
        runningTerminal = nil
        let position = tabs.firstIndex { $0.terminalId == terminalId } ?? tabs.count
        let before = tabs.prefix(position).filter(\.running)
        let others = tabs.filter { $0.running && $0.terminalId != terminalId }
        if tab != nil {
            model.perform(.closeTerminal(threadId: threadId, terminalId: terminalId))
        }
        if let next = before.last ?? others.first {
            switchTo(next.terminalId)
        } else {
            dismiss()
        }
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
        keys.capture = { [weak view] in
            view?.terminalStateSnapshot().visibleRows
                .map { $0.text.replacingOccurrences(of: "\\s+$", with: "", options: .regularExpression) }
                .joined(separator: "\n") ?? ""
        }
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
