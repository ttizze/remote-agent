import AgentCore
import SwiftUI

struct ConversationWorkspacePicker: View {
    let model: ConversationModel
    @State private var choosingBase = false

    var body: some View {
        if let selection = model.composerControls.workspace {
            HStack(spacing: 12) {
                Menu {
                    Button("ローカル") { select(.root) }
                    if selection.isRepository {
                        Button("ワークツリー") {
                            select(.worktree(baseRef: selection.baseRef, startFromOrigin: selection.startFromOrigin))
                        }
                        ForEach(selection.worktrees, id: \.self) { path in
                            Button(path) { select(.existingWorktree(path: path)) }
                        }
                    }
                } label: { Text(verbatim: selection.label) }
                if selection.mode == "worktree" {
                    Button { choosingBase = true } label: {
                        Label(
                            selection.baseRef.isEmpty ? "ベースブランチ" : selection.baseRef,
                            systemImage: "arrow.triangle.branch"
                        )
                        .lineLimit(1)
                    }
                    Toggle("リモートから開始", isOn: Binding(
                        get: { selection.startFromOrigin },
                        set: { select(.worktree(baseRef: selection.baseRef, startFromOrigin: $0)) }
                    )).toggleStyle(.button)
                }
            }
            .font(.caption)
            .sheet(isPresented: $choosingBase) {
                WorkspaceBasePicker(selection: selection) { base in
                    select(.worktree(baseRef: base, startFromOrigin: selection.startFromOrigin))
                }
            }
        }
    }

    private func select(_ workspace: WorkspaceStrategy) {
        model.perform(.selectWorkspace(threadId: model.coreDraftKey, workspace: workspace))
    }
}

private struct WorkspaceBasePicker: View {
    let selection: WorkspaceSelection
    let select: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""

    var body: some View {
        NavigationStack {
            List {
                TextField("ブランチ・タグ・コミット", text: $query).textInputAutocapitalization(.never).autocorrectionDisabled()
                ForEach(
                    selection.refs.filter { query.isEmpty || $0.localizedCaseInsensitiveContains(query) },
                    id: \.self
                ) { reference in
                    Button(reference) { select(reference); dismiss() }
                }
            }
            .navigationTitle("ベースブランチ")
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("選択") { select(query); dismiss() }
                        .disabled(query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
                ToolbarItem(placement: .cancellationAction) { Button("キャンセル") { dismiss() } }
            }
        }.onAppear { query = selection.baseRef }
    }
}
