import AgentCore
import SwiftUI

/// "Archived Threads".
struct ArchivedScreen: View {
    @ObservedObject var model: BexAppViewModel
    @State private var query = ""
    @State private var confirming: (ArchivedRow, ArchivedAction)?

    var body: some View {
        let view = model.snapshot.archived(
            nowMs: Int64(Date().timeIntervalSince1970 * 1000),
            options: ArchivedOptions(layout: .screen, searchQuery: query, sortOrder: .newest, confirmDelete: true)
        )
        List {
            if let error = view.error {
                NoticeText(text: error)
            }
            if let empty = view.empty {
                EmptyStateText(title: empty.title, detail: empty.detail)
            }
            ForEach(view.groups, id: \.projectId) { group in
                Section(group.title) {
                    ForEach(group.rows, id: \.threadId) { row in
                        VStack(alignment: .leading, spacing: 3) {
                            Text(row.title).font(AppTheme.font(16, weight: .medium))
                            Text(row.description).font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                        }
                        .swipeActions {
                            ForEach(Array(row.actions.enumerated()), id: \.offset) { _, action in
                                Button(action.label, role: action.destructive ? .destructive : nil) {
                                    run(row, action)
                                }
                            }
                        }
                        .contextMenu {
                            ForEach(Array(row.actions.enumerated()), id: \.offset) { _, action in
                                Button(action.label, role: action.destructive ? .destructive : nil) {
                                    run(row, action)
                                }
                            }
                        }
                    }
                }
            }
        }
        .refreshable { model.perform(.refresh) }
        .searchable(text: $query)
        .scrollContentBackground(.hidden)
        .background(AppTheme.color("surfaceOverlay"))
        .navigationTitle("Archived Threads")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { model.perform(.showArchived(open: true)) }
        .onDisappear { model.perform(.showArchived(open: false)) }
        .alert(confirming?.1.confirmation?.title ?? "", isPresented: Binding(
            get: { confirming != nil }, set: {
                if !$0 {
                    confirming = nil
                }
            }
        )) {
            if let (row, action) = confirming {
                Button(action.label, role: .destructive) {
                    model.perform(.thread(threadId: row.threadId, action: action.action))
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(confirming?.1.confirmation?.message ?? "")
        }
    }

    private func run(_ row: ArchivedRow, _ action: ArchivedAction) {
        if action.confirmation != nil {
            confirming = (row, action)
        } else {
            model.perform(.thread(threadId: row.threadId, action: action.action))
        }
    }
}
