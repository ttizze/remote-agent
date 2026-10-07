import AgentCore
import SwiftUI

/// "Arrange threads": every thread under its Pinned, Active, Snoozed or
/// Settled header. Dragging a row reorders, pins, unpins or settles it; the
/// change saves when the row drops.
struct ThreadArrangementSheet: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss
    @State private var snoozedExpanded = false
    @State private var settledExpanded = false

    private var options: ArrangementOptions {
        ArrangementOptions(snoozedExpanded: snoozedExpanded, settledExpanded: settledExpanded)
    }

    private var nowMs: Int64 {
        Int64(Date().timeIntervalSince1970 * 1000)
    }

    var body: some View {
        let view = model.snapshot.threadArrangement(nowMs: nowMs, options: options)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 12) {
                Text("Arrange threads").font(AppTheme.font(21, weight: .semibold)).foregroundStyle(AppTheme.text)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button("Done") { dismiss() }.font(AppTheme.font(16))
                    .foregroundStyle(AppTheme.color("mobilePrimaryText"))
                    .frame(minHeight: 44).padding(.horizontal, 12)
            }
            .padding(.leading, 20).padding(.trailing, 8).padding(.vertical, 12)
            Text("Drag to reorder, pin, or settle. Changes save when you drop.").font(AppTheme.font(14))
                .foregroundStyle(AppTheme.muted).padding(.horizontal, 20).padding(.bottom, 12)
            List {
                ForEach(view.rows, id: \.key) { row in
                    rowView(row, locked: view.locked)
                        .listRowInsets(EdgeInsets(top: 0, leading: 20, bottom: 0, trailing: 8))
                        .listRowBackground(AppTheme.screen)
                        .listRowSeparatorTint(AppTheme.borderSubtle)
                }
                .onMove { source, destination in drop(view, from: source, to: destination) }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .environment(\.editMode, .constant(.active))
        }
        .background(AppTheme.screen.ignoresSafeArea())
    }

    @ViewBuilder
    private func rowView(_ row: ArrangementRow, locked: Bool) -> some View {
        switch row.kind {
        case let .header(label, foldable, _):
            Button {
                if row.section == .snoozed {
                    snoozedExpanded.toggle()
                } else if row.section == .settled {
                    settledExpanded.toggle()
                }
            } label: {
                Text(label).font(AppTheme.font(14, weight: .semibold)).foregroundStyle(AppTheme.muted)
                    .frame(maxWidth: .infinity, minHeight: 48, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(!foldable)
            .moveDisabled(true)
        case let .thread(threadId, title, canMoveUp, canMoveDown, sectionMoves):
            Text(title).font(AppTheme.font(16)).foregroundStyle(AppTheme.text).lineLimit(2)
                .frame(maxWidth: .infinity, minHeight: 56, alignment: .leading)
                .moveDisabled(locked)
                .accessibilityLabel("Reorder \(title)")
                .accessibilityHint(
                    "Move up and Move down reorder within this section. Other actions move between sections."
                )
                .accessibilityActions {
                    ForEach(sectionMoves, id: \.label) { move in
                        Button(move.label) { send(threadId, section: move.section) }
                    }
                    if canMoveUp {
                        Button("Move up") { step(threadId, row.section, upward: true) }
                    }
                    if canMoveDown {
                        Button("Move down") { step(threadId, row.section, upward: false) }
                    }
                }
        }
    }

    /// The list moved a row to `to`; core says where that lands, if anywhere.
    private func drop(_ view: ThreadArrangementView, from: IndexSet, to destination: Int) {
        guard let index = from.first, case let .thread(threadId, _, _, _, _) = view.rows[index].kind,
              let landing = model.snapshot.threadArrangementMove(
                  nowMs: nowMs, options: options, threadId: threadId, toIndex: UInt32(destination)
              ) else { return }
        Haptics.light()
        model.perform(.moveThread(threadId: threadId, section: landing.section, destination: landing.destination))
    }

    private func send(_ threadId: String, section: DropSection) {
        Haptics.light()
        model.perform(.moveThread(
            threadId: threadId, section: section == .pinned ? .pinned : .active,
            destination: .drop(target: nil, section: section, placement: .before)
        ))
    }

    private func step(_ threadId: String, _ section: DragSection, upward: Bool) {
        Haptics.light()
        model.perform(.moveThread(
            threadId: threadId, section: section == .pinned ? .pinned : .active,
            destination: upward ? .up : .down
        ))
    }
}
