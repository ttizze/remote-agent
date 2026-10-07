import AgentCore
import SwiftUI

/// "Review changes": the thread's turn diffs and worktree changes.
struct ReviewScreen: View {
    @ObservedObject var model: BexAppViewModel
    @State private var files: [WorkspaceDiffFile] = []
    @State private var loading = false
    @State private var error: String?
    @State private var selectedFile: String?

    var body: some View {
        let diff = model.threadView?.diff
        Group {
            if loading {
                VStack(spacing: 10) {
                    ProgressView()
                    Text("Loading diff…").font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let error {
                EmptyStateText(title: "Review unavailable", detail: error)
            } else if let message = diff?.emptyMessage, diff?.request == nil {
                EmptyStateText(title: "No review diffs", detail: message)
            } else if files.isEmpty {
                EmptyStateText(title: "No changes", detail: "This diff is empty.")
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ChangedFilesList(files: files, selected: $selectedFile)
                        ForEach(visibleFiles, id: \.path) { file in
                            DiffFileView(file: file)
                        }
                    }
                }
                .refreshable { load() }
            }
        }
        .background(AppTheme.screen.ignoresSafeArea())
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 1) {
                    Text(diff?.sectionTitle ?? "Review changes").font(AppTheme.font(17, weight: .heavy))
                    if !files.isEmpty {
                        Text(subtitle).font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
                    }
                }
            }
            ToolbarItem(placement: .topBarTrailing) {
                if let diff {
                    ScopeMenu(diff: diff) { intent in
                        model.perform(intent)
                        load()
                    }
                }
            }
        }
        .onAppear(perform: load)
    }

    private var visibleFiles: [WorkspaceDiffFile] {
        guard let selectedFile else { return files }
        return files.filter { $0.path == selectedFile }
    }

    private var subtitle: String {
        let added = files.reduce(0) { $0 + ($1.additions ?? 0) }
        let removed = files.reduce(0) { $0 + ($1.deletions ?? 0) }
        return "+\(added) · −\(removed)"
    }

    private func load() {
        loading = true
        error = nil
        model.requestSnapshot(.loadDiff) { snapshot, result in
            if case let .failure(failure) = result {
                loading = false
                error = failure.localizedDescription
                return
            }
            guard let review = snapshot.review() else {
                loading = false
                files = []
                return
            }
            Task {
                let loaded = await Task.detached(priority: .userInitiated) { review.diffFiles() }.value
                files = loaded
                selectedFile = nil
                loading = false
            }
        }
    }
}

private struct ScopeMenu: View {
    let diff: DiffPanelView
    let select: (Intent) -> Void

    var body: some View {
        Menu {
            ForEach(Array(diff.scopes.enumerated()), id: \.offset) { _, scope in
                Button { select(.selectDiffScope(choice: scope.choice)) } label: {
                    if scope.selected {
                        Label(scope.label, systemImage: "checkmark")
                    } else {
                        Text(scope.label)
                    }
                }
            }
            if !diff.turns.isEmpty {
                Menu("Turn") {
                    ForEach(diff.turns, id: \.runId) { turn in
                        Button { select(.selectDiffTurn(runId: turn.runId, filePath: nil)) } label: {
                            if turn.selected {
                                Label(turn.label, systemImage: "checkmark")
                            } else {
                                Text(turn.label)
                            }
                        }
                    }
                }
            }
            Toggle(diff.whitespaceToggleLabel, isOn: Binding(get: { diff.ignoreWhitespace }, set: {
                select(.setDiffIgnoreWhitespace(ignore: $0))
            }))
        } label: {
            Image(systemName: "ellipsis")
        }
        .accessibilityLabel("Select diff")
    }
}

private struct ChangedFilesList: View {
    let files: [WorkspaceDiffFile]
    @Binding var selected: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("Changed files").font(AppTheme.font(13, weight: .medium)).foregroundStyle(AppTheme.muted)
                .padding(.horizontal, 10).padding(.top, 8)
            ForEach(files, id: \.path) { file in
                Button { selected = selected == file.path ? nil : file.path } label: {
                    HStack {
                        Text(file.path).font(AppTheme.font(13, weight: selected == file.path ? .bold : .medium))
                            .foregroundStyle(AppTheme.text).lineLimit(2)
                        Spacer()
                        if let additions = file.additions {
                            Text("+\(additions)").foregroundStyle(AppTheme.emerald)
                        }
                        if let deletions = file.deletions {
                            Text("-\(deletions)").foregroundStyle(AppTheme.roseText)
                        }
                    }
                    .font(AppTheme.font(12, weight: .bold))
                    .padding(.horizontal, 10).frame(minHeight: 42)
                    .background(selected == file.path ? AppTheme.subtleStrong : .clear,
                                in: RoundedRectangle(cornerRadius: 10.5))
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 8).padding(.bottom, 12)
    }
}

private struct DiffFileView: View {
    let file: WorkspaceDiffFile

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(file.path).font(AppTheme.mono(12, weight: .bold)).padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(AppTheme.color("surfaceRaised"))
            ScrollView(.horizontal) {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(file.rows.enumerated()), id: \.offset) { _, row in
                        HStack(spacing: 6) {
                            Text(row.old.map(String.init) ?? "").frame(width: 34, alignment: .trailing)
                            Text(row.new.map(String.init) ?? "").frame(width: 34, alignment: .trailing)
                            Text(row.text).fixedSize()
                        }
                        .font(AppTheme.mono(12))
                        .foregroundStyle(row.kind == "@" ? AppTheme.muted : AppTheme.text)
                        .frame(minHeight: 22, alignment: .leading)
                        .background(background(row.kind))
                    }
                }
            }
        }
        .padding(.bottom, 12)
    }

    private func background(_ kind: String) -> Color {
        switch kind {
        case "+": AppTheme.emerald.opacity(0.12)
        case "-": AppTheme.rose.opacity(0.12)
        default: .clear
        }
    }
}
