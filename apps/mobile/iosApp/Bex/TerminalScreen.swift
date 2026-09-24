import AgentCore
import SwiftTerm
import SwiftUI

struct TerminalScreen: View {
    @ObservedObject var model: BexAppViewModel
    let cwd: String
    private var handle: String {
        terminalHandle(cwd: cwd)
    }

    var body: some View {
        VStack(spacing: 0) {
            let terminal = model.snapshot.terminalView(handle: handle)
            if terminal?.loading ?? true {
                ProgressView().accessibilityIdentifier("terminal.loading")
            } else if let status = terminal?.status {
                Text(status).font(.caption).foregroundStyle(.secondary)
            }
            NativeTerminalView(model: model, handle: handle, cwd: cwd, terminal: terminal)
        }
    }
}

private struct NativeTerminalView: UIViewRepresentable {
    @ObservedObject var model: BexAppViewModel
    let handle: String
    let cwd: String
    let terminal: AgentCore.TerminalView?

    func makeCoordinator() -> Coordinator {
        Coordinator(model: model, handle: handle, cwd: cwd)
    }

    func makeUIView(context: Context) -> SwiftTerm.TerminalView {
        let view = HostTerminalView(frame: .zero)
        view.terminalDelegate = context.coordinator
        view.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        view.accessibilityIdentifier = "terminal.screen"
        return view
    }

    static func dismantleUIView(_ view: SwiftTerm.TerminalView, coordinator: Coordinator) {
        view.terminalDelegate = nil
        coordinator.dismantled = true
        if coordinator.started {
            coordinator.model.perform(.detachTerminal(DetachTerminal(handle: coordinator.handle)))
        }
    }

    func updateUIView(_ view: SwiftTerm.TerminalView, context: Context) {
        let coordinator = context.coordinator
        guard let terminal else { return }
        view.accessibilityValue = terminal.acceptsInput && !terminal.loading ? "入力可能" : nil
        let previousSequence = coordinator.sequence
        for chunk in terminal.output where chunk.sequence > coordinator.sequence {
            if let size = chunk.resetSize {
                coordinator.restoring = true
                view.resize(cols: Int(size.cols), rows: Int(size.rows))
            }
            view.feed(byteArray: Array(chunk.data)[...])
            coordinator.restoring = false
            coordinator.sequence = chunk.sequence
        }
        if coordinator.sequence > previousSequence {
            let sequence = coordinator.sequence
            Task { @MainActor in model.perform(.acknowledgeTerminal(handle: handle, sequence: sequence)) }
        }
    }

    @MainActor final class Coordinator: NSObject, TerminalViewDelegate {
        let model: BexAppViewModel
        let handle: String
        var sequence: UInt64 = 0
        var restoring = false
        var started = false
        var dismantled = false
        let cwd: String
        init(model: BexAppViewModel, handle: String, cwd: String) {
            self.model = model; self.handle = handle; self.cwd = cwd
        }

        func send(source _: SwiftTerm.TerminalView, data: ArraySlice<UInt8>) {
            guard !dismantled, model.snapshot.terminalView(handle: handle)?.acceptsInput == true else { return }
            model.perform(.writeTerminal(WriteTerminal(handle: handle, data: Data(data))))
        }

        func sizeChanged(source _: SwiftTerm.TerminalView, newCols: Int, newRows: Int) {
            guard !dismantled, !restoring, newCols > 0, newRows > 0 else { return }
            let size = TerminalSize(
                cols: UInt16(clamping: min(newCols, 500)),
                rows: UInt16(clamping: min(newRows, 250))
            )
            Task { @MainActor in
                guard !dismantled else { return }
                if !started {
                    started = true
                    model.perform(.startTerminal(StartTerminal(handle: handle, cwd: cwd, size: size)))
                } else if model.snapshot.terminalView(handle: handle)?.acceptsInput == true {
                    model.perform(.resizeTerminal(ResizeTerminal(handle: handle, size: size)))
                }
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
    @TaskLocal private nonisolated static var isPasting = false

    override func paste(_ sender: Any?) {
        // SwiftTerm sends paste bytes synchronously through the emulator delegate.
        // Scope forwarding to this call, without enabling replies from its parser thread.
        Self.$isPasting.withValue(true) { super.paste(sender) }
    }

    override nonisolated func send(source: SwiftTerm.Terminal, data: ArraySlice<UInt8>) {
        guard Self.isPasting else { return }
        super.send(source: source, data: data)
    }
}
