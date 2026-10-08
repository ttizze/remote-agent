import AgentCore
import SwiftUI
import UIKit

/// Git status, actions, pull request creation and pull request checkout for a
/// thread's Host-owned checkout.
struct GitOverviewSheet: View {
    @ObservedObject var model: BexAppViewModel
    let cwd: String
    let review: () -> Void
    let mergeBack: () -> Void
    let mergeBackAvailable: Bool
    @Environment(\.dismiss) private var dismiss
    @State private var actionId: String?
    @State private var pendingAction: GitAction?
    @State private var showingConfirmation = false
    @State private var commitMessage = ""
    @State private var excludedFiles = Set<String>()
    @State private var editingFiles = false
    @State private var pullRequestReference = ""
    @State private var checkoutMode = "worktree"
    @State private var newBranchName = ""
    @State private var worktreeBaseBranch = ""
    @State private var worktreeBranchName = ""

    private var status: GitStatus? {
        model.snapshot.gitStatus(cwd: cwd)
    }

    private var localRefs: [GitRef] {
        model.snapshot.gitRefs(cwd: cwd).filter { !$0.isRemote }
    }

    private var menu: [GitActionMenuItem] {
        model.snapshot.gitMenu(cwd: cwd)
    }

    private var progress: GitActionProgress? {
        guard let actionId else { return nil }
        return model.snapshot.gitAction(actionId: actionId)
    }

    private var allFiles: [GitFileChange] {
        status?.workingTree ?? []
    }

    private var selectedFiles: [GitFileChange] {
        allFiles.filter { !excludedFiles.contains($0.path) }
    }

    var body: some View {
        NavigationStack {
            List {
                GitStatusSection(
                    status: status,
                    mergeBackAvailable: mergeBackAvailable,
                    onReview: { review(); dismiss() },
                    onMergeBack: { mergeBack(); dismiss() },
                    onRefresh: refresh
                )
                GitFilesSection(
                    status: status,
                    excludedFiles: $excludedFiles,
                    editingFiles: $editingFiles
                )
                GitActionSection(
                    menu: menu,
                    selectedFilesEmpty: selectedFiles.isEmpty,
                    quickPullAvailable: model.snapshot.gitQuickAction(cwd: cwd)?.kind == .pull,
                    commitMessage: $commitMessage,
                    onAction: handle,
                    onPull: { model.perform(.pullVcs(cwd: cwd)) }
                )
                GitBranchSection(
                    status: status,
                    localRefs: localRefs,
                    newBranchName: $newBranchName,
                    worktreeBaseBranch: $worktreeBaseBranch,
                    worktreeBranchName: $worktreeBranchName,
                    cwd: cwd,
                    onInitialize: {
                        model.perform(.initRepository(cwd: cwd)) { _ in subscribe() }
                    },
                    onCreateBranch: createBranch,
                    onCreateWorktree: createWorktree,
                    onSwitchRef: switchRef,
                    onRemoveWorktree: { path in
                        model.perform(.removeVcsWorktree(cwd: cwd, path: path, force: false)) { _ in
                            refreshRefs()
                        }
                    }
                )
                GitPullRequestSection(
                    pullRequestReference: $pullRequestReference,
                    checkoutMode: $checkoutMode,
                    onCheckout: checkoutPullRequest
                )
                GitProgressSection(progress: progress)
            }
            .listStyle(.insetGrouped)
            .navigationTitle("Git")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .confirmationDialog(
                confirmationTitle,
                isPresented: $showingConfirmation,
                titleVisibility: .visible
            ) {
                if let copy = confirmationCopy {
                    Button(copy.continueLabel) { runPending(featureBranch: false) }
                    Button("Feature branch & continue") { runPending(featureBranch: true) }
                }
                Button("Cancel", role: .cancel) { pendingAction = nil }
            } message: {
                Text(confirmationCopy?.description ?? "Choose how to continue.")
            }
            .onAppear { subscribe() }
            .onChange(of: cwd) { _, _ in
                excludedFiles.removeAll()
                editingFiles = false
                subscribe()
            }
        }
    }

    private var confirmationCopy: DefaultBranchActionCopy? {
        guard let pendingAction,
              let branch = status?.refName,
              model.snapshot.gitRequiresDefaultBranchConfirmation(cwd: cwd, action: pendingAction.name)
        else { return nil }
        return model.snapshot.gitDefaultBranchActionCopy(
            cwd: cwd,
            action: pendingAction.name,
            includesCommit: pendingAction.includesCommit
        ) ?? DefaultBranchActionCopy(
            title: "Run action on default ref?",
            description: "This action runs on \"\(branch)\".",
            continueLabel: "Continue"
        )
    }

    private var confirmationTitle: String {
        confirmationCopy?.title ?? "Run Git action?"
    }

    private func subscribe() {
        guard !cwd.isEmpty else { return }
        model.perform(.subscribeVcsStatus(cwd: cwd))
        model.perform(.loadVcsRefs(cwd: cwd, query: ""))
        model.perform(.refreshVcsStatus(cwd: cwd))
    }

    private func refresh() {
        model.perform(.refreshVcsStatus(cwd: cwd))
        refreshRefs()
    }

    private func refreshRefs() {
        model.perform(.loadVcsRefs(cwd: cwd, query: ""))
    }

    private func createBranch() {
        let name = newBranchName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return }
        model.perform(.createVcsRef(cwd: cwd, refName: name)) { _ in
            newBranchName = ""
            refreshRefs()
            refresh()
        }
    }

    private func createWorktree() {
        let base = worktreeBaseBranch.trimmingCharacters(in: .whitespacesAndNewlines)
        let branch = worktreeBranchName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !base.isEmpty, !branch.isEmpty else { return }
        model.perform(.createVcsWorktree(
            cwd: cwd,
            refName: base,
            newRefName: branch,
            baseRefName: base,
            path: nil
        )) { _ in
            worktreeBranchName = ""
            refreshRefs()
        }
    }

    private func switchRef(_ ref: GitRef) {
        model.perform(.switchVcsRef(cwd: cwd, refName: ref.name)) { _ in
            refreshRefs()
            refresh()
        }
    }

    private func handle(_ action: GitAction) {
        if action == .openPr {
            if let value = status?.pullRequestUrl, let url = URL(string: value) {
                UIApplication.shared.open(url)
            }
            return
        }
        pendingAction = action
        if confirmationCopy != nil {
            showingConfirmation = true
        } else {
            runPending(featureBranch: false)
        }
    }

    private func runPending(featureBranch: Bool) {
        guard let action = pendingAction, action != .openPr else { return }
        let id = "ios-git-\(UUID().uuidString)"
        actionId = id
        pendingAction = nil
        showingConfirmation = false
        model.perform(.runVcsAction(
            actionId: id,
            cwd: cwd,
            action: action.name,
            commitMessage: commitMessage.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ? nil
                : commitMessage.trimmingCharacters(in: .whitespacesAndNewlines),
            featureBranch: featureBranch,
            filePaths: excludedFiles.isEmpty ? nil : selectedFiles.map(\.path),
            threadId: model.snapshot.selectedThreadId(),
            projectId: model.snapshot.selectedProjectId()
        ))
    }

    private func checkoutPullRequest() {
        let reference = pullRequestReference.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !reference.isEmpty else { return }
        model.perform(.preparePullRequestThread(
            cwd: cwd,
            reference: reference,
            mode: checkoutMode,
            threadId: model.snapshot.selectedThreadId()
        )) { result in
            guard case let .success(.gitPullRequestThreadPrepared(value)) = result else { return }
            let project = model.snapshot.selectedProjectId() ?? "chats"
            model.perform(.newThreadOnBranch(
                projectId: project,
                branch: value.branch,
                worktreePath: value.worktreePath
            ))
            dismiss()
        }
    }
}
