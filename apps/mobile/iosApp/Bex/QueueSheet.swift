import AgentCore
import SwiftUI

/// "Queued": the messages waiting for the current turn.
struct QueueSheet: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let queue = model.threadView?.queue
        let rows = queue?.rows ?? []
        NavigationStack {
            List {
                if let notice = queue?.heldNotice {
                    HStack(spacing: 7) {
                        Text(notice).font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
                        Spacer()
                        if queue?.canResume == true {
                            Button("Resume queue") { model.perform(.queue(action: .resume)) }
                                .font(AppTheme.font(14, weight: .medium))
                        }
                    }
                    .padding(.vertical, 10.5)
                    .listRowBackground(Color.clear)
                }
                if rows.isEmpty {
                    Text("No messages waiting in this queue.").font(AppTheme.font(14))
                        .foregroundStyle(AppTheme.muted).frame(maxWidth: .infinity).padding(.top, 21)
                        .listRowBackground(Color.clear).listRowSeparator(.hidden)
                }
                ForEach(Array(rows.enumerated()), id: \.element.key) { index, row in
                    QueueRow(row: row, canPromote: queue?.canPromoteToSteer == true, actions: actions(index, rows))
                        .listRowBackground(Color.clear)
                        .listRowInsets(EdgeInsets(top: 0, leading: 17.5, bottom: 0, trailing: 17.5))
                }
                .onMove(perform: queue?.canReorder == true && rows.count > 1 ? { move(rows, from: $0, to: $1) } : nil)
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .background(AppTheme.color("surfaceOverlay"))
            .navigationTitle("Queued")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    Text("Queued").font(AppTheme.font(18, weight: .heavy))
                }
            }
        }
        .presentationDetents([.fraction(0.65), .fraction(0.95)])
        .presentationDragIndicator(.visible)
        .onChange(of: rows.isEmpty) { _, empty in
            if empty {
                dismiss()
            }
        }
    }

    private func actions(_ index: Int, _ rows: [QueueRowView]) -> QueueRowActions {
        let row = rows[index]
        let runId = row.runId
        return QueueRowActions(
            steer: {
                if let runId {
                    model.perform(.queue(action: .steer(runId: runId)))
                }
            },
            edit: {
                if let runId {
                    model.perform(.queue(action: .edit(runId: runId)))
                    dismiss()
                }
            },
            moveUp: {
                guard let runId, index > 0 else { return }
                model.perform(.queue(action: .move(runId: runId, beforeRunId: rows[index - 1].runId)))
            },
            moveDown: {
                guard let runId, index + 1 < rows.count else { return }
                let before = index + 2 < rows.count ? rows[index + 2].runId : nil
                model.perform(.queue(action: .move(runId: runId, beforeRunId: before)))
            },
            remove: {
                if let runId {
                    model.perform(.queue(action: .cancel(runId: runId)))
                }
            }
        )
    }

    private func move(_ rows: [QueueRowView], from: IndexSet, to destination: Int) {
        guard let source = from.first, let runId = rows[source].runId,
              destination != source, destination != source + 1 else { return }
        let before = destination < rows.count ? rows[destination].runId : nil
        guard before != runId else { return }
        model.perform(.queue(action: .move(runId: runId, beforeRunId: before)))
    }
}

struct QueueRowActions {
    let steer: () -> Void
    let edit: () -> Void
    let moveUp: () -> Void
    let moveDown: () -> Void
    let remove: () -> Void
}

private struct QueueRow: View {
    let row: QueueRowView
    let canPromote: Bool
    let actions: QueueRowActions

    var body: some View {
        HStack(spacing: 8.75) {
            if !row.thumbnails.isEmpty {
                HStack(spacing: 3.5) {
                    ForEach(row.thumbnails.prefix(3), id: \.attachmentId) { _ in
                        Image(systemName: "photo").font(.system(size: 11)).frame(width: 24, height: 24)
                            .background(AppTheme.subtleStrong, in: RoundedRectangle(cornerRadius: 4))
                    }
                    if let overflow = row.compactOverflow {
                        Text(overflow).font(AppTheme.font(11)).foregroundStyle(AppTheme.muted)
                    }
                }
            }
            Text(row.title).font(AppTheme.font(14)).lineLimit(1)
                .foregroundStyle(row.editing ? AppTheme.muted : AppTheme.text)
            if row.editing {
                Text("EDITING").font(AppTheme.font(12)).foregroundStyle(AppTheme.primary)
            }
            Spacer(minLength: 0)
            if canPromote, row.controls.canSteer {
                Button(action: actions.steer) {
                    Text("Steer").font(AppTheme.font(13, weight: .medium)).foregroundStyle(.white)
                        .padding(.horizontal, 10.5).frame(height: 28)
                        .background(AppTheme.primary, in: Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(row.steerAccessibilityLabel)
            }
        }
        .frame(minHeight: 49)
        .padding(.vertical, 8.75)
        .contentShape(Rectangle())
        .onTapGesture {
            if row.controls.canEdit {
                actions.edit()
            }
        }
        .accessibilityHint("Opens this message in the composer for editing")
        .contextMenu {
            if canPromote, row.controls.canSteer {
                Button("Steer now", systemImage: "arrow.turn.left.up", action: actions.steer)
            }
            if row.controls.canEdit {
                Button("Edit", systemImage: "pencil", action: actions.edit)
            }
            Button("Move up", action: actions.moveUp).disabled(!row.controls.canMoveUp)
            Button("Move down", action: actions.moveDown).disabled(!row.controls.canMoveDown)
            if row.controls.canDismiss {
                Button("Remove", role: .destructive, action: actions.remove)
            }
        }
        .swipeActions(edge: .trailing, allowsFullSwipe: true) {
            if row.controls.canDismiss {
                Button(role: .destructive, action: actions.remove) { Label("Remove", systemImage: "trash") }
                    .accessibilityLabel(row.controls.dismissAccessibilityLabel)
            }
        }
    }
}
