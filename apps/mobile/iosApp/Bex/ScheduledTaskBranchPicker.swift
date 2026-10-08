import AgentCore
import SwiftUI

struct ScheduledTaskBranchPicker: View {
    @ObservedObject var model: BexAppViewModel
    @Binding var draft: ScheduledTaskDraft
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""

    private var baseBranch: String {
        if case let .worktree(base, _, _) = draft.workspace {
            return base
        }
        return ""
    }

    private var branchView: ScheduledTaskBranchView {
        model.snapshot.scheduledTaskBranches(
            projectId: draft.projectId,
            selectedBranch: baseBranch
        )
    }

    var body: some View {
        List {
            if branchView.loading {
                ProgressView("Loading branches…")
            }
            if let error = branchView.error {
                Text(error).foregroundStyle(AppTheme.muted)
            }
            ForEach(branchView.branches, id: \.name) { branch in
                Button {
                    select(branch.name)
                } label: {
                    HStack {
                        Image(systemName: "arrow.triangle.branch")
                            .foregroundStyle(AppTheme.muted)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(branch.name).foregroundStyle(AppTheme.text)
                            if let badge = branch.badge {
                                Text(badge.uppercased())
                                    .font(AppTheme.font(12))
                                    .foregroundStyle(AppTheme.muted)
                            }
                        }
                        Spacer()
                        if branch.selected {
                            Image(systemName: "checkmark")
                                .foregroundStyle(AppTheme.primary)
                        }
                    }
                }
            }
            if branchView.hasMore {
                Button("Load more branches") {
                    model.perform(.loadMoreScheduledTaskBranches(projectId: draft.projectId))
                }
            }
            if branchView.branches.isEmpty, !branchView.loading, branchView.error == nil {
                Text("No local branches available").foregroundStyle(AppTheme.muted)
            }
        }
        .navigationTitle("Base branch")
        .searchable(text: $query, prompt: "Find a branch")
        .onAppear {
            model.perform(.searchScheduledTaskBranches(projectId: draft.projectId, query: query))
        }
        .onChange(of: query) { _, value in
            model.perform(.searchScheduledTaskBranches(projectId: draft.projectId, query: value))
        }
    }

    private func select(_ branch: String) {
        guard case let .worktree(_, currentBranch, startFromOrigin) = draft.workspace else {
            return
        }
        draft.workspace = .worktree(
            baseRef: branch,
            branch: currentBranch,
            startFromOrigin: startFromOrigin
        )
        dismiss()
    }
}
