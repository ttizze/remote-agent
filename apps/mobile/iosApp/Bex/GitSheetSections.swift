import AgentCore
import SwiftUI

struct GitStatusSection: View {
    let status: GitStatus?
    let mergeBackAvailable: Bool
    let onReview: () -> Void
    let onMergeBack: () -> Void
    let onRefresh: () -> Void

    var body: some View {
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
                Button(action: onRefresh) {
                    Image(systemName: "arrow.clockwise")
                }
                .accessibilityLabel("Refresh Git status")
            }
            Button(action: onReview) {
                Label("Review changes", systemImage: "text.bubble")
            }
            if mergeBackAvailable {
                Button(action: onMergeBack) {
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

    private var statusDetail: String {
        guard let status else { return "Connect to a Host to load Git status." }
        if !status.isRepo {
            return "This folder is not a Git repository."
        }
        var parts = [String]()
        if status.hasWorkingTreeChanges {
            parts.append("Changes")
        }
        if status.aheadCount > 0 {
            parts.append("\(status.aheadCount) ahead")
        }
        if status.behindCount > 0 {
            parts.append("\(status.behindCount) behind")
        }
        if parts.isEmpty {
            parts.append(status.hasUpstream ? "Up to date" : "No upstream")
        }
        return parts.joined(separator: " · ")
    }
}

struct GitFilesSection: View {
    let status: GitStatus?
    @Binding var excludedFiles: Set<String>
    @Binding var editingFiles: Bool

    private var allFiles: [GitFileChange] {
        status?.workingTree ?? []
    }

    private var selectedFiles: [GitFileChange] {
        allFiles.filter { !excludedFiles.contains($0.path) }
    }

    private var selectedInsertions: UInt64 {
        selectedFiles.reduce(0) { $0 + $1.insertions }
    }

    private var selectedDeletions: UInt64 {
        selectedFiles.reduce(0) { $0 + $1.deletions }
    }

    var body: some View {
        Section("Files") {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("\(selectedFiles.count) selected")
                        .font(AppTheme.font(14, weight: .semibold))
                    Text("+\(selectedInsertions) / -\(selectedDeletions)")
                        .font(AppTheme.font(12))
                        .foregroundStyle(AppTheme.muted)
                }
                Spacer()
                if !allFiles.isEmpty {
                    if !excludedFiles.isEmpty {
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
}

struct GitActionSection: View {
    let menu: [GitActionMenuItem]
    let selectedFilesEmpty: Bool
    let quickPullAvailable: Bool
    @Binding var commitMessage: String
    let onAction: (GitAction) -> Void
    let onPull: () -> Void

    var body: some View {
        Section("Actions") {
            TextField("Commit message (optional)", text: $commitMessage, axis: .vertical)
                .lineLimit(2 ... 4)
            if menu.isEmpty {
                Text("Git status is unavailable.")
                    .foregroundStyle(AppTheme.muted)
            } else {
                ForEach(menu, id: \.id) { item in
                    Button { onAction(item.action) } label: {
                        Label(item.label, systemImage: item.action.symbol)
                    }
                    .disabled(item.disabled || (selectedFilesEmpty && item.action.includesCommit))
                }
            }
            if quickPullAvailable {
                Button(action: onPull) {
                    Label("Pull", systemImage: "arrow.down")
                }
            }
        }
    }
}

struct GitBranchSection: View {
    let status: GitStatus?
    let localRefs: [GitRef]
    @Binding var newBranchName: String
    @Binding var worktreeBaseBranch: String
    @Binding var worktreeBranchName: String
    let cwd: String
    let onInitialize: () -> Void
    let onCreateBranch: () -> Void
    let onCreateWorktree: () -> Void
    let onSwitchRef: (GitRef) -> Void
    let onRemoveWorktree: (String) -> Void

    var body: some View {
        Section("Branches & worktrees") {
            if status?.isRepo == false {
                Button(action: onInitialize) {
                    Label("Initialize repository", systemImage: "folder.badge.plus")
                }
            } else {
                TextField("New branch name", text: $newBranchName)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button(action: onCreateBranch) {
                    Label("Create & checkout", systemImage: "plus")
                }
                .disabled(newBranchName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

                TextField("Base branch", text: $worktreeBaseBranch)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .onAppear {
                        if worktreeBaseBranch.isEmpty {
                            worktreeBaseBranch = status?.refName ?? "main"
                        }
                    }
                TextField("New worktree branch", text: $worktreeBranchName)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button(action: onCreateWorktree) {
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
                            Button { onSwitchRef(ref) } label: {
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
                                    onRemoveWorktree(path)
                                }
                                .font(AppTheme.font(12))
                            }
                        }
                    }
                }
            }
        }
    }

    private func refDetail(_ ref: GitRef) -> String {
        if ref.current {
            return "Checked out here"
        }
        if let path = ref.worktreePath {
            return "Checked out in \(path)"
        }
        if ref.isDefault {
            return "Default branch"
        }
        return "Local branch"
    }
}

struct GitProgressSection: View {
    let progress: GitActionProgress?

    var body: some View {
        if let progress {
            Section("Git action") {
                if let label = progress.label {
                    Text(label)
                }
                Text(progress.status.replacingOccurrences(of: "_", with: " ").capitalized)
                    .foregroundStyle(progress.error == nil ? AppTheme.muted : .red)
                if let output = progress.output, !output.isEmpty {
                    Text(output).font(AppTheme.mono(12))
                }
                if let error = progress.error {
                    Text(error).foregroundStyle(.red)
                }
            }
        }
    }
}
