import AgentCore
import SwiftUI

/// The terminal's visible output, to attach a range of its lines to the
/// thread's composer. Line numbers count the frozen viewport, not the scrollback.
struct TerminalContextSheet: View {
    let lines: [String]
    let terminalId: String
    let terminalLabel: String
    let attach: (TerminalOutputContext) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var start = 0
    @State private var end: Int
    @State private var anchor: Int?

    init(lines: [String], terminalId: String, terminalLabel: String,
         attach: @escaping (TerminalOutputContext) -> Void) {
        self.lines = lines
        self.terminalId = terminalId
        self.terminalLabel = terminalLabel
        self.attach = attach
        _end = State(initialValue: max(0, lines.count - 1))
    }

    var body: some View {
        let selection = visibleOutputSelection(lines: lines, start: UInt32(start), end: UInt32(end))
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Visible terminal output").font(AppTheme.font(18)).foregroundStyle(AppTheme.text)
                Spacer()
                Button("Cancel") { dismiss() }.font(AppTheme.font(16)).foregroundStyle(AppTheme.text).padding(12)
            }
            .padding(.leading, 16).padding(.trailing, 4).padding(.vertical, 4)
            Text("Tap the first and last line to select a range.").font(AppTheme.font(16))
                .foregroundStyle(AppTheme.muted).padding(.horizontal, 16).padding(.bottom, 12)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { index, line in
                        lineRow(index, line)
                    }
                }
                .padding(16)
            }
            if selection.tooLarge {
                Text("Select fewer lines to fit the context limit.").font(AppTheme.font(16))
                    .foregroundStyle(AppTheme.muted).padding(.horizontal, 16)
            }
            Button {
                attach(TerminalOutputContext(
                    terminalId: terminalId, terminalLabel: terminalLabel,
                    lineStart: UInt32(start + 1), lineEnd: UInt32(end + 1), text: selection.text
                ))
            } label: {
                Text("Attach selected output").font(AppTheme.font(16)).foregroundStyle(AppTheme.text)
                    .frame(maxWidth: .infinity).padding(16)
                    .background(AppTheme.subtle, in: RoundedRectangle(cornerRadius: 12))
            }
            .buttonStyle(.plain)
            .disabled(!selection.canAttach)
            .opacity(selection.canAttach ? 1 : 0.5)
            .padding(.horizontal, 16).padding(.top, 16).padding(.bottom, 40)
        }
        .background(AppTheme.sheet.ignoresSafeArea())
    }

    private func lineRow(_ index: Int, _ line: String) -> some View {
        let selected = index >= start && index <= end
        return Button {
            if let first = anchor {
                start = min(first, index)
                end = max(first, index)
                anchor = nil
            } else {
                anchor = index
                start = index
                end = index
            }
        } label: {
            Text("\(index + 1) \(line.isEmpty ? " " : line)").font(AppTheme.mono(14))
                .foregroundStyle(AppTheme.text)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
                .background(selected ? AppTheme.subtle : Color.clear)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Line \(index + 1): \(line)")
        .accessibilityAddTraits(selected ? [.isSelected] : [])
    }
}

/// Sticky modifiers and keys the software keyboard lacks.
@MainActor
final class TerminalKeys: ObservableObject {
    @Published var control = false
    @Published var alt = false
    var send: ((Data) -> Void)?
    var paste: (() -> Void)?
    /// The terminal's visible rows as text.
    var capture: (() -> String)?

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

struct KeyBar: View {
    @ObservedObject var keys: TerminalKeys
    let clear: () -> Void

    var body: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 6) {
                key("esc", [0x1B])
                toggle("CTRL", $keys.control)
                toggle("ALT", $keys.alt)
                key("tab", [0x09])
                Button("paste") { keys.paste?() }.buttonStyle(KeyStyle(active: false))
                Button("CLEAR", action: clear).buttonStyle(KeyStyle(active: false))
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
