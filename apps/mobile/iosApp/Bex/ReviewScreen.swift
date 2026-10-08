import AgentCore
import SwiftUI

/// "Review changes": the thread's turn diffs and worktree changes.
struct ReviewScreen: View {
    @ObservedObject var model: BexAppViewModel
    @State private var files: [WorkspaceDiffFile] = []
    /// While the diff shows file by file: each file's notice by path.
    @State private var notices: [String: String]?
    /// The latest parse; an older one finishing late is dropped.
    @State private var parsed = 0
    @State private var loading = false
    @State private var error: String?
    @State private var selectedFile: String?

    var body: some View {
        let diff = model.threadView?.diff
        let git = showsGitDiff(diff) ? diff?.git : nil
        Group {
            if loading || git?.loading == true {
                VStack(spacing: 10) {
                    ProgressView()
                    Text("Loading diff…").font(AppTheme.font(14)).foregroundStyle(AppTheme.muted)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let error = error ?? git?.error {
                EmptyStateText(title: "Review unavailable", detail: error)
            } else if let message = diff?.emptyMessage, diff?.request == nil {
                EmptyStateText(title: "No review diffs", detail: message)
            } else if files.isEmpty {
                EmptyStateText(title: "No changes", detail: git.map(emptyDetail) ?? "This diff is empty.")
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        if git?.truncated == true {
                            PartialDiffNotice()
                        }
                        ChangedFilesList(files: files, selected: $selectedFile) { path in
                            if notices != nil {
                                model.perform(.revealDiffFile(path: path, retry: true))
                            }
                        }
                        ForEach(visibleFiles, id: \.path) { file in
                            DiffFileView(file: file, notice: notices?[file.path])
                                .onAppear {
                                    if notices != nil {
                                        model.perform(.revealDiffFile(path: file.path, retry: false))
                                    }
                                }
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
        .onChange(of: model.snapshot.reviewRevision()) { _, _ in parse(model.snapshot) }
        .onChange(of: git?.filesRevision) { _, _ in parse(model.snapshot) }
    }

    private func showsGitDiff(_ diff: DiffPanelView?) -> Bool {
        diff?.scopes.contains { $0.selected && ($0.choice == .branch || $0.choice == .unstaged) } == true
    }

    private func emptyDetail(_ git: GitDiffView) -> String {
        if !git.isRepo {
            return "Turn diffs are unavailable because this project is not a git repository."
        }
        return git.subtitle ?? "This diff is empty."
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
            parse(snapshot)
        }
    }

    private func parse(_ snapshot: AgentCore.Snapshot) {
        parsed += 1
        let generation = parsed
        // A diff too large to send whole lists every file and reads each one on its own.
        if let thread = snapshot.selectedThreadId(), model.threadView?.diff.git?.filesRevision != nil {
            Task {
                let lazy = await Task.detached(priority: .userInitiated) {
                    snapshot.reviewFiles(threadId: thread)
                }.value
                guard let lazy, generation == parsed else { return }
                let listed = lazy.files.map {
                    WorkspaceDiffFile(path: $0.path, additions: $0.additions, deletions: $0.deletions, rows: $0.rows)
                }
                if listed.map(\.path) != files.map(\.path) || notices == nil {
                    selectedFile = nil
                }
                files = listed
                let listedNotices = lazy.files.compactMap { file in file.notice.map { (file.path, $0) } }
                notices = Dictionary(listedNotices) { first, _ in first }
                loading = false
            }
            return
        }
        notices = nil
        guard let review = snapshot.review() else {
            loading = false
            files = []
            return
        }
        Task {
            let loaded = await Task.detached(priority: .userInitiated) { review.diffFiles() }.value
            guard generation == parsed else { return }
            files = loaded
            selectedFile = nil
            loading = false
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
                .disabled(diff.git?.isRepo == false && (scope.choice == .branch || scope.choice == .unstaged))
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

/// A diff cut at the Host's size cap.
private struct PartialDiffNotice: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("PARTIAL DIFF").font(AppTheme.font(12, weight: .bold))
            Text("Diff output hit the server size cap. Showing the available excerpt.").font(AppTheme.font(12))
        }
        .foregroundStyle(AppTheme.color("mobileWarningForeground"))
        .padding(.horizontal, 16).padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(AppTheme.color("mobileWarning"))
        .overlay(alignment: .bottom) { Rectangle().fill(AppTheme.color("mobileWarningBorder")).frame(height: 1) }
    }
}

private struct ChangedFilesList: View {
    let files: [WorkspaceDiffFile]
    @Binding var selected: String?
    let select: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("Changed files").font(AppTheme.font(13, weight: .medium)).foregroundStyle(AppTheme.muted)
                .padding(.horizontal, 10).padding(.top, 8)
            ForEach(files, id: \.path) { file in
                Button {
                    selected = selected == file.path ? nil : file.path
                    select(file.path)
                } label: {
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
    /// Why the file shows no or partial rows, below them.
    let notice: String?

    var body: some View {
        let wrapping = AppTheme.codeWordWrap
        VStack(alignment: .leading, spacing: 0) {
            Text(file.path).font(AppTheme.mono(13, weight: .bold)).padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(AppTheme.cardAlt)
            ScrollView(wrapping ? .vertical : .horizontal) {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(file.rows.enumerated()), id: \.offset) { _, row in
                        HStack(spacing: 6) {
                            Text(row.old.map(String.init) ?? "")
                                .font(.custom("Menlo", size: AppTheme.codeLineNumberFontSize))
                                .frame(width: 34, alignment: .trailing)
                            Text(row.new.map(String.init) ?? "")
                                .font(.custom("Menlo", size: AppTheme.codeLineNumberFontSize))
                                .frame(width: 34, alignment: .trailing)
                            Text(row.text).font(AppTheme.mono(13))
                                .fixedSize(horizontal: !wrapping, vertical: false)
                                .frame(maxWidth: wrapping ? .infinity : nil, alignment: .leading)
                        }
                        .foregroundStyle(row.kind == "@" ? AppTheme.muted : AppTheme.text)
                        .frame(maxWidth: wrapping ? .infinity : nil, minHeight: AppTheme.codeLineHeight, alignment: .leading)
                        .background(background(row.kind))
                    }
                }
            }
            if let notice {
                HStack(spacing: 10) {
                    Image(systemName: "info.circle").font(.system(size: 14))
                    Text(notice).font(AppTheme.font(12)).lineLimit(1)
                }
                .foregroundStyle(AppTheme.muted)
                .padding(.horizontal, 10)
                .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                .overlay(alignment: .bottom) { Rectangle().fill(AppTheme.border.opacity(0.65)).frame(height: 1) }
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
