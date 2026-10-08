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

    private var status: GitStatus? { model.snapshot.gitStatus(cwd: cwd) }
    private var localRefs: [GitRef] {
        model.snapshot.gitRefs(cwd: cwd).filter { !$0.isRemote }
    }
    private var menu: [GitActionMenuItem] { model.snapshot.gitMenu(cwd: cwd) }
    private var progress: GitActionProgress? {
        guard let actionId else { return nil }
        return model.snapshot.gitAction(actionId: actionId)
    }

    var body: some View {
        NavigationStack {
            List {
                statusSection
                fileSelectionSection
                actionSection
                branchSection
                checkoutSection
                progressSection
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

    @ViewBuilder
    private var branchSection: some View {
        Section("Branches & worktrees") {
            if status?.isRepo == false {
                Button {
                    model.perform(.initRepository(cwd: cwd)) { _ in subscribe() }
                } label: {
                    Label("Initialize repository", systemImage: "folder.badge.plus")
                }
            } else {
                TextField("New branch name", text: $newBranchName)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button {
                    createBranch()
                } label: {
                    Label("Create & checkout", systemImage: "plus")
                }
                .disabled(newBranchName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

                TextField("Base branch", text: $worktreeBaseBranch)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .onAppear {
                        if worktreeBaseBranch.isEmpty { worktreeBaseBranch = status?.refName ?? "main" }
                    }
                TextField("New worktree branch", text: $worktreeBranchName)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button {
                    createWorktree()
                } label: {
                    Label("Create worktree", systemImage: "square.split.2x1")
                }
                .disabled(
                    worktreeBaseBranch.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || worktreeBranchName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                )

                if localRefs.isEmpty {
                    Text("Loading branches…")
                        .foregroundStyle(AppTheme.muted)
                } else {
                    ForEach(localRefs, id: \.name) { ref in
                        VStack(alignment: .leading, spacing: 4) {
                            Button {
                                switchRef(ref)
                            } label: {
                                HStack {
                                    Image(systemName: ref.current ? "checkmark.circle.fill" : "arrow.branch")
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(ref.name)
                                        Text(refDetail(ref))
                                            .font(AppTheme.font(11))
                                            .foregroundStyle(AppTheme.muted)
                                    }
                                    Spacer()
                                }
                            }
                            .disabled(ref.current || ref.worktreePath != nil)
                            if let path = ref.worktreePath, path != cwd {
                                Button("Remove worktree", role: .destructive) {
                                    model.perform(.removeVcsWorktree(cwd: cwd, path: path, force: false)) { _ in
                                        refreshRefs()
                                    }
                                }
                                .font(AppTheme.font(12))
                            }
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var statusSection: some View {
        Section("Checkout") {
            HStack {
                Image(systemName: "arrow.branch")
                    .foregroundStyle(AppTheme.primary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(status?.refName ?? "Select ref")
                        .font(AppTheme.font(16, weight: .semibold))
                    Text(statusDetail)
                        .font(AppTheme.font(12))
                        .foregroundStyle(AppTheme.muted)
                }
                Spacer()
                Button { refresh() } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .accessibilityLabel("Refresh Git status")
            }
            Button { review(); dismiss() } label: {
                Label("Review changes", systemImage: "text.bubble")
            }
            if mergeBackAvailable {
                Button { mergeBack(); dismiss() } label: {
                    Label("Merge back to source", systemImage: "arrow.triangle.merge")
                }
            }
            if let url = status?.pullRequestUrl, let link = URL(string: url) {
                Link(destination: link) {
                    Label("Open pull request", systemImage: "arrow.up.right.square")
                }
            }
            if status?.isDefaultRef == true {
                Label("Default ref", systemImage: "exclamationmark.triangle")
                    .foregroundStyle(AppTheme.muted)
            }
        }
    }

    private var allFiles: [GitFileChange] { status?.workingTree ?? [] }
    private var selectedFiles: [GitFileChange] {
        allFiles.filter { !excludedFiles.contains($0.path) }
    }
    private var allFilesSelected: Bool { excludedFiles.isEmpty }

    @ViewBuilder
    private var fileSelectionSection: some View {
        Section("Files") {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("\(selectedFiles.count) selected")
                        .font(AppTheme.font(14, weight: .semibold))
                    Text("+\(selectedFiles.reduce(0) { $0 + $1.insertions }) / -\(selectedFiles.reduce(0) { $0 + $1.deletions })")
                        .font(AppTheme.font(12))
                        .foregroundStyle(AppTheme.muted)
                }
                Spacer()
                if !allFiles.isEmpty {
                    if !allFilesSelected {
                        Button("Reset") { excludedFiles.removeAll() }
                            .font(AppTheme.font(12))
                    }
                    Button(editingFiles ? "Done" : "Edit") { editingFiles.toggle() }
                        .font(AppTheme.font(12))
                }
            }
            if allFiles.isEmpty {
                Text("No changed files are available to commit.")
                    .foregroundStyle(AppTheme.muted)
            } else if editingFiles {
                ForEach(allFiles, id: \.path) { file in
                    Button { toggleFile(file.path) } label: {
                        HStack {
                            Image(systemName: excludedFiles.contains(file.path) ? "square" : "checkmark.square.fill")
                            Text(file.path)
                                .font(AppTheme.mono(12))
                                .lineLimit(1)
                            Spacer()
                            Text("+\(file.insertions)  -\(file.deletions)")
                                .font(AppTheme.mono(11))
                                .foregroundStyle(AppTheme.muted)
                        }
                    }
                }
            } else {
                ForEach(selectedFiles.prefix(3), id: \.path) { file in
                    HStack {
                        Text(file.path)
                            .font(AppTheme.mono(12))
                            .lineLimit(1)
                        Spacer()
                        Text("+\(file.insertions)  -\(file.deletions)")
                            .font(AppTheme.mono(11))
                            .foregroundStyle(AppTheme.muted)
                    }
                }
                if selectedFiles.count > 3 {
                    Text("+\(selectedFiles.count - 3) more files")
                        .font(AppTheme.font(12))
                        .foregroundStyle(AppTheme.muted)
                }
            }
        }
    }

    private func toggleFile(_ path: String) {
        if excludedFiles.contains(path) {
            excludedFiles.remove(path)
        } else {
            excludedFiles.insert(path)
        }
    }

    @ViewBuilder
    private var actionSection: some View {
        Section("Actions") {
            TextField("Commit message (optional)", text: $commitMessage, axis: .vertical)
                .lineLimit(2...4)
            if menu.isEmpty {
                Text("Git status is unavailable.")
                    .foregroundStyle(AppTheme.muted)
            } else {
                ForEach(menu, id: \.id) { item in
                    Button {
                        handle(item.action)
                    } label: {
                        Label(item.label, systemImage: item.action.symbol)
                    }
                    .disabled(item.disabled || (selectedFiles.isEmpty && item.action.includesCommit))
                }
            }
            if let quick = model.snapshot.gitQuickAction(cwd: cwd), quick.kind == .pull {
                Button { model.perform(.pullVcs(cwd: cwd)) } label: {
                    Label("Pull", systemImage: "arrow.down")
                }
            }
        }
    }

    @ViewBuilder
    private var checkoutSection: some View {
        Section("Pull request thread") {
            TextField("PR number or URL", text: $pullRequestReference)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Picker("Checkout", selection: $checkoutMode) {
                Text("Worktree").tag("worktree")
                Text("Current checkout").tag("local")
            }
            Button {
                checkoutPullRequest()
            } label: {
                Label("Open pull request as thread", systemImage: "arrow.branch")
            }
            .disabled(pullRequestReference.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    @ViewBuilder
    private var progressSection: some View {
        if let progress {
            Section("Git action") {
                if let label = progress.label { Text(label) }
                Text(progress.status.replacingOccurrences(of: "_", with: " ").capitalized)
                    .foregroundStyle(progress.error == nil ? AppTheme.muted : .red)
                if let output = progress.output, !output.isEmpty {
                    Text(output).font(AppTheme.mono(12))
                }
                if let error = progress.error { Text(error).foregroundStyle(.red) }
            }
        }
    }

    private var statusDetail: String {
        guard let status else { return "Connect to a Host to load Git status." }
        if !status.isRepo { return "This folder is not a Git repository." }
        var parts = [String]()
        if status.hasWorkingTreeChanges { parts.append("Changes") }
        if status.aheadCount > 0 { parts.append("\(status.aheadCount) ahead") }
        if status.behindCount > 0 { parts.append("\(status.behindCount) behind") }
        if parts.isEmpty { parts.append(status.hasUpstream ? "Up to date" : "No upstream") }
        return parts.joined(separator: " · ")
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

    private func refDetail(_ ref: GitRef) -> String {
        if ref.current { return "Checked out here" }
        if let path = ref.worktreePath { return "Checked out in \(path)" }
        if ref.isDefault { return "Default branch" }
        return "Local branch"
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
            filePaths: allFilesSelected ? nil : selectedFiles.map(\.path),
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

private extension GitAction {
    var name: String {
        switch self {
        case .commit: "commit"
        case .push: "push"
        case .createPr: "create_pr"
        case .commitPush: "commit_push"
        case .commitPushPr: "commit_push_pr"
        case .openPr: "open_pr"
        }
    }

    var includesCommit: Bool {
        self == .commit || self == .commitPush || self == .commitPushPr
    }

    var symbol: String {
        switch self {
        case .commit: "checkmark.circle"
        case .push: "arrow.up"
        case .createPr, .commitPushPr, .openPr: "arrow.triangle.pull"
        case .commitPush: "arrow.up.circle"
        }
    }
}
