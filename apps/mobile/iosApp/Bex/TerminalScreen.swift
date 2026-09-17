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
            Text(model.snapshot.terminalView(handle: handle)?.status ?? "接続中…")
                .font(.caption).foregroundStyle(.secondary)
            NativeTerminalView(model: model, handle: handle, cwd: cwd)
        }
        .onDisappear { model.perform(.detachTerminal(DetachTerminal(handle: handle))) }
    }
}

private struct NativeTerminalView: UIViewRepresentable {
    @ObservedObject var model: BexAppViewModel
    let handle: String
    let cwd: String

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

    func updateUIView(_ view: SwiftTerm.TerminalView, context: Context) {
        let coordinator = context.coordinator
        guard let terminal = model.snapshot.terminalView(handle: handle) else { return }
        for chunk in terminal.output where chunk.sequence > coordinator.sequence {
            if let size = chunk.resetSize {
                coordinator.restoring = true
                view.resize(cols: Int(size.cols), rows: Int(size.rows))
            }
            view.feed(byteArray: Array(chunk.data)[...])
            coordinator.restoring = false
            coordinator.sequence = chunk.sequence
            let sequence = chunk.sequence
            Task { @MainActor in model.perform(.acknowledgeTerminal(handle: handle, sequence: sequence)) }
        }
    }

    @MainActor final class Coordinator: NSObject, TerminalViewDelegate {
        let model: BexAppViewModel
        let handle: String
        var sequence: UInt64 = 0
        var restoring = false
        var started = false
        let cwd: String
        init(model: BexAppViewModel, handle: String, cwd: String) {
            self.model = model; self.handle = handle; self.cwd = cwd
        }

        func send(source _: SwiftTerm.TerminalView, data: ArraySlice<UInt8>) {
            model.perform(.writeTerminal(WriteTerminal(handle: handle, data: Data(data))))
        }

        func sizeChanged(source _: SwiftTerm.TerminalView, newCols: Int, newRows: Int) {
            guard !restoring, newCols > 0, newRows > 0 else { return }
            let size = TerminalSize(
                cols: UInt16(clamping: min(newCols, 500)),
                rows: UInt16(clamping: min(newRows, 250))
            )
            if !started {
                started = true
                Task { @MainActor in
                    model.perform(.startTerminal(StartTerminal(handle: handle, cwd: cwd, size: size)))
                }
            } else {
                Task { @MainActor in model.perform(.resizeTerminal(ResizeTerminal(handle: handle, size: size))) }
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
    override nonisolated func send(source _: SwiftTerm.Terminal, data _: ArraySlice<UInt8>) {}
}
