import AgentCore
import SwiftUI

/// "Branch" / "Base branch": the searchable branch list of the new task's project.
struct BranchPicker: View {
    @ObservedObject var model: BexAppViewModel
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var switching = false
    @State private var switchError: String?

    var body: some View {
        let workspace = model.snapshot.newThread(options: ComposerOptions(
            compact: true, alternateModifier: false,
            shortcuts: ComposerShortcuts(alternateSend: nil, queueSteer: nil, queueEdit: nil)
        )).workspace
        let branches = workspace?.branches ?? []
        ScrollView {
            LazyVStack(spacing: 12) {
                if let workspace, workspace.mode == .worktree {
                    Toggle(isOn: Binding(get: { workspace.startFromOrigin }, set: {
                        model.perform(.setNewThreadStartFromOrigin(on: $0))
                    })) {
                        Text("Start from origin").font(AppTheme.font(16, weight: .medium))
                            .foregroundStyle(AppTheme.text)
                    }
                    .padding(.horizontal, 16).frame(minHeight: 56)
                    .background(AppTheme.groupedCard, in: RoundedRectangle(cornerRadius: 16))
                }
                if branches.isEmpty {
                    emptyState(workspace)
                } else {
                    LazyVStack(spacing: 0) {
                        ForEach(Array(branches.enumerated()), id: \.offset) { index, branch in
                            Button { select(branch) } label: {
                                BranchRow(branch: branch, last: index == branches.count - 1)
                            }
                            .buttonStyle(.plain)
                            .disabled(switching)
                            .opacity(switching ? 0.45 : 1)
                        }
                    }
                    .background(AppTheme.groupedCard, in: RoundedRectangle(cornerRadius: 16))
                    .clipShape(RoundedRectangle(cornerRadius: 16))
                    if workspace?.branchesLoadingMore == true {
                        ProgressView().padding(.vertical, 16)
                    } else if workspace?.hasMoreBranches == true {
                        Color.clear.frame(height: 1)
                            .onAppear { model.perform(.loadMoreNewThreadBranches) }
                    }
                }
            }
            .padding(16)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(AppTheme.sheet.ignoresSafeArea())
        .navigationTitle(workspace?.branchRole ?? "Branch")
        .navigationBarTitleDisplayMode(.inline)
        .searchable(text: $query, prompt: "Find a branch")
        .onChange(of: query) { _, value in model.perform(.searchNewThreadBranches(query: value)) }
        .onAppear { model.perform(.searchNewThreadBranches(query: "")) }
        .alert("Could not switch branch", isPresented: Binding(get: { switchError != nil }, set: {
            if !$0 {
                switchError = nil
            }
        })) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(switchError ?? "")
        }
    }

    @ViewBuilder
    private func emptyState(_ workspace: NewThreadWorkspaceView?) -> some View {
        let loading = workspace?.branchesLoading ?? false
        VStack(spacing: 12) {
            if loading {
                ProgressView()
            }
            Text(loading ? "Loading branches…" : workspace?.branchError
                ?? (query.isEmpty ? "No branches available" : "No matching branches"))
                .font(AppTheme.font(14)).foregroundStyle(AppTheme.muted).multilineTextAlignment(.center)
            if !loading, workspace?.branchError != nil {
                Button { model.perform(.searchNewThreadBranches(query: query)) } label: {
                    Text("Try again").font(AppTheme.font(14, weight: .medium)).foregroundStyle(AppTheme.text)
                        .padding(.horizontal, 16).padding(.vertical, 8)
                        .background(AppTheme.card, in: Capsule())
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 16).padding(.top, 120)
        .frame(maxWidth: .infinity)
    }

    /// A local branch checked out nowhere is switched to first; the picker
    /// stays open when that fails.
    private func select(_ branch: BranchChoice) {
        switching = true
        model.perform(.selectNewThreadBranch(branch: branch.name, worktreePath: branch.worktreePath)) { result in
            switching = false
            switch result {
            case .success:
                dismiss()
            case let .failure(error):
                guard !(error is CancellationError) else { return }
                switchError = model.snapshot.error() ?? error.localizedDescription
            }
        }
    }
}

private struct BranchRow: View {
    let branch: BranchChoice
    let last: Bool

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "arrow.triangle.branch").font(.system(size: 17))
                .foregroundStyle(AppTheme.color("mobileIconMuted"))
            VStack(alignment: .leading, spacing: 2) {
                Text(branch.name).font(AppTheme.font(16, weight: .medium)).foregroundStyle(AppTheme.text).lineLimit(1)
                if let badge = branch.badge {
                    Text(badge.uppercased()).font(AppTheme.font(12)).foregroundStyle(AppTheme.muted).lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if branch.selected {
                Image(systemName: "checkmark").font(.system(size: 16, weight: .semibold))
                    .foregroundStyle(AppTheme.color("mobileIcon"))
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 12)
        .frame(minHeight: 56)
        .overlay(alignment: .bottom) {
            if !last {
                Rectangle().fill(AppTheme.borderSubtle).frame(height: 1)
            }
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(branch.selected ? .isSelected : [])
    }
}
