import AgentCore
import SwiftUI
import UIKit

struct WorkspaceDiffScreen: View {
    let model: ConversationModel
    let root: String
    @State private var files: [WorkspaceDiffFile]?

    var body: some View {
        // Core runs only the newest review read; an older request can finish
        // first, so the view follows the snapshot rather than its own request.
        let review = model.files.review()
        Group {
            if let error = model.conversationContext.workspaceError() {
                VStack {
                    AppNotice(text: error).padding()
                    Button("再試行", action: loadDiff).accessibilityIdentifier("files.diff.retry")
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let files, !files.isEmpty {
                ScrollView {
                    LazyVStack(spacing: 16) {
                        ForEach(files, id: \.path) { file in
                            WorkspaceDiffCard(file: file)
                        }
                    }.padding(12)
                }
            } else if files != nil {
                ContentUnavailableView("変更はありません", systemImage: "checkmark.circle")
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .onAppear(perform: loadDiff)
        .task(id: ReviewIdentity(review: review)) {
            guard let review else { return }
            let next = await Task.detached(priority: .userInitiated) { review.diffFiles() }.value
            guard !Task.isCancelled else { return }
            files = next
        }
    }

    private func loadDiff() {
        files = nil
        model.perform(.reviewWorkspace(ReviewWorkspace(cwd: root)))
    }
}

private struct ReviewIdentity: Equatable {
    let review: WorkspaceReview?

    static func == (lhs: Self, rhs: Self) -> Bool {
        guard let left = lhs.review, let right = rhs.review else { return lhs.review == nil && rhs.review == nil }
        return left.unchanged(other: right)
    }
}

struct WorkspaceDiffCard: View {
    let file: WorkspaceDiffFile
    @State private var expanded = true

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            VStack(spacing: 0) {
                if file.rows.isEmpty {
                    Text("テキスト差分はありません").font(.caption).foregroundColor(.secondary).padding()
                }
                ForEach(file.rows.indices, id: \.self) { index in
                    let row = file.rows[index]
                    HStack(alignment: .top, spacing: 0) {
                        if row.kind == "+" || row.kind == "-" || row.kind == " " {
                            Text((row.new ?? row.old).map(String.init) ?? "")
                                .foregroundColor(.secondary).frame(width: 40, alignment: .trailing).padding(
                                    .trailing,
                                    8
                                )
                        }
                        Text(row.text.isEmpty ? " " : row.text)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .fixedSize(horizontal: false, vertical: true)
                            .textSelection(.enabled)
                    }
                    .font(.system(.footnote, design: .monospaced))
                    .padding(.vertical, 3).padding(.horizontal, 8)
                    .background(row.kind == "+" ? Color.green.opacity(0.18) :
                        row.kind == "-" ? Color.red.opacity(0.18) :
                        row.kind == "@" ? Color.secondary.opacity(0.12) : Color.clear)
                }
            }
            .background(Color(uiColor: .systemBackground))
        } label: {
            HStack(alignment: .top, spacing: 8) {
                Text(file.path).fontWeight(.semibold).frame(maxWidth: .infinity, alignment: .leading)
                if let additions = file.additions {
                    Text("+\(additions)").foregroundColor(.green)
                }
                if let deletions = file.deletions {
                    Text("−\(deletions)").foregroundColor(.red)
                }
            }
            .font(.subheadline)
            .foregroundStyle(.primary)
            .accessibilityIdentifier("diff.file.\(file.path)")
        }
        .padding(12)
        .background(Color(uiColor: .secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 12))
    }
}

struct WorkspaceAgentsScreen: View {
    let agents: [AgentSummary]

    var body: some View {
        if agents.isEmpty {
            ContentUnavailableView("サブエージェントが起動するとここに表示されます。", systemImage: "network")
        } else {
            agentList
        }
    }

    private var agentList: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(agents, id: \.key) { agent in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            if agent.active {
                                ProgressView().controlSize(.small)
                            }
                            Text(agent.title)
                            Spacer()
                            AppText(agent.status).foregroundStyle(.secondary)
                        }
                        if let detail = agent.detail {
                            Text(detail).foregroundStyle(.secondary)
                        }
                        if let model = agent.model {
                            Text(model).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .padding(12).background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
                    .padding(.leading, CGFloat(agent.depth) * 16)
                }
            }.font(.subheadline).padding(16).frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
