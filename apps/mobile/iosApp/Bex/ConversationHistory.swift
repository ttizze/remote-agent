import AgentCore
import SwiftUI
import UIKit

extension ConversationModel {
    func mediaAccess(presentation: ConversationMediaPresentation,
                     open: @escaping (WorkspaceDestination) -> Void) -> ConversationMediaAccess {
        let draft = coreDraftKey
        let cwd = cwd
        return ConversationMediaAccess(
            host: host,
            cwd: cwd,
            home: hostHomeDirectory,
            draft: draft,
            presentation: presentation,
            download: { [self] path in
                return try await download(path)
            },
            visualization: { [self] path in
                let result = try await outcome(for: .loadVisualization(LoadVisualization(path: path, cwd: cwd)))
                guard case let .visualization(html) = result else { throw URLError(.badServerResponse) }
                return html
            },
            resolve: { [self, home = hostHomeDirectory] source in
                try await resolveConversationFile(source, cwd: cwd, home: home)
            },
            openDirectory: { path in
                guard self.coreDraftKey == draft else { return }
                open(WorkspaceDestination(
                    scope: self.snapshot.panelScope(host: self.host, draft: draft), directory: path
                ))
            },
            openFile: { path, line in
                guard self.coreDraftKey == draft else { return }
                open(WorkspaceDestination(
                    scope: self.snapshot.panelScope(host: self.host, draft: draft), directory: cwd, filePath: path,
                    fileLine: line
                ))
            }
        )
    }

    private func resolveConversationFile(_ source: String, cwd: String,
                                         home: String?) async throws -> ConversationMediaAccess.ResolvedFile {
        guard let request = mobileFileOpenRequest(source: source, cwd: cwd, home: home)
        else { throw URLError(.unsupportedURL) }
        let result = try await outcome(for: .resolveFileReference(request))
        guard case let .fileReferenceResolved(path, directory) = result
        else { throw URLError(.badServerResponse) }
        return ConversationMediaAccess.ResolvedFile(
            path: path, isDirectory: directory,
            line: markdownFileTarget(source: source, cwd: cwd, surface: .mobile)?.line
        )
    }
}

/// History and row presentation
extension ThreadScreen {
    var mediaAccess: ConversationMediaAccess {
        model.mediaAccess(presentation: mediaPresentation) { openedDirectory = $0 }
    }

    func historyControl(_ control: HistoryControl, session: SessionRef) -> some View {
        VStack(spacing: 6) {
            if control.hasMore {
                Button {
                    scroll.following = false
                    model.loadOlderHistory(session)
                } label: {
                    HStack(spacing: 12) {
                        Rectangle().frame(height: 1).foregroundStyle(Color(UIColor.separator))
                        AppText(control.loadLabel).fixedSize()
                        Rectangle().frame(height: 1).foregroundStyle(Color(UIColor.separator))
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.vertical, 8)
                    .contentShape(Rectangle())
                }
                .disabled(control.loading)
                .accessibilityIdentifier("history.load")
            }
            if let error = control.error {
                ErrorTextView(text: error).font(.caption).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity)
                    .multilineTextAlignment(.center)
                    .accessibilityIdentifier("history.error")
            }
        }
        .padding(.bottom, 8)
    }

    @ViewBuilder
    func conversationRow(_ row: ThreadConversationRow) -> some View {
        switch row.content {
        case let .historyNotice(message): AppText(message).font(.caption).foregroundStyle(.secondary)
        case let .native(content, item): nativeConversationRow(content, item: item)
        case let .queued(item):
            userMessageRow(item, timestampMs: item.data.timestampMs)
        }
    }

    @ViewBuilder
    func nativeConversationRow(_ row: ConversationRow, item: ConversationItem?) -> some View {
        switch (row.content, item) {
        case let (.preparation(preparation), _):
            ThreadPreparationRow(preparation: preparation) { model.perform($0) }
        case let (.providerHandoff(text), _):
            Text(text).font(.caption).foregroundStyle(.secondary).padding(.vertical, 8)
        case let (.user, item?):
            userMessageRow(item, timestampMs: row.timestampMs)
                .padding(
                    .top,
                    16
                )
        case (let .response(_, showActions, turnId), let item?):
            ThreadMessageRow(
                item: item,
                isUser: false,
                media: mediaAccess,
                selection: selectionActions,
                showActions: showActions,
                timestampMs: row.timestampMs,
                fork: turnId
                    .map { id in { model.forkAndOpen(through: id) { _ in } } },
                forking: turnId.map { model.conversationContext.forkPhase(turn: $0) == .running } ?? false,
                forkError: turnId.flatMap { model.conversationContext.forkError(turn: $0) }
            )
        case let (.activityHeader(activity), _): activityHeader(activity)
        case (let .activity(_, turnId), let item?): activityItem(item, turnId: turnId)
        case let (.error(error), _): ThreadErrorRow(error: error)
        case (.inProgress, _), (.pendingRequest, _), (_, nil): EmptyView()
        }
    }

    func userMessageRow(_ item: ConversationItem, timestampMs: UInt64? = nil) -> some View {
        ThreadMessageRow(
            item: item,
            isUser: true,
            media: mediaAccess,
            selection: selectionActions,
            timestampMs: timestampMs,
            restoreUnknown: { model.restoreUnknownSubmission($0) },
            discardUnknown: { model.discardUnknownSubmission($0) }
        )
    }

    func activityItem(_ item: ConversationItem, turnId: String) -> some View {
        ThreadItemRow(
            item: item,
            media: mediaAccess,
            selection: selectionActions,
            isExpanded: expandedItemIds.contains(item.data.id),
            toggleExpanded: {
                scroll.following = false
                if expandedItemIds.contains(item.data.id) {
                    expandedItemIds.remove(item.data.id)
                } else {
                    expandedItemIds.insert(item.data.id)
                }
            },
            loadDetails: {
                guard let threadId = conversation?.id else { return nil }
                return await model.readItemDetails(
                    threadId: threadId, turnId: turnId,
                    itemId: item.data.nativeId ?? item.data.id
                )
            }
        ).equatable()
    }

    @ViewBuilder
    func activityHeader(_ turn: ActivityPresentation) -> some View {
        let expanded = AgentCore.activityIsExpanded(
            status: turn.status,
            expandedStatus: activityExpansionOverrides[turn.id]
        )
        let header = ThreadActivityHeader(summary: turn.activitySummary, canCollapse: turn.activityCanCollapse,
                                          isInProgress: turn.isInProgress, expanded: expanded)
        if turn.activityCanCollapse {
            Button {
                scroll.following = false
                activityExpansionOverrides[turn.id] = expanded ? nil : turn.status
                if !expanded, let params = turn.loadItems {
                    model.perform(.loadTurnItems(params))
                }
            } label: {
                header
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("turn.activity." + turn.id)
        } else {
            header.accessibilityIdentifier("turn.activity." + turn.id)
        }
    }

    func threadAccessibilityValue(_ thread: ConversationPresentation) -> String {
        let itemCount = thread.rows.filter { row in
            guard case let .native(native, _) = row.content else { return false }
            return switch native.content {
            case .user, .activity, .response, .pendingRequest, .error: true
            default: false
            }
        }.count
        return "turns=\(thread.source.turnCount());items=\(itemCount)"
    }
}

private struct ThreadPreparationRow: View {
    let preparation: PreparationPresentation
    let perform: (Intent) -> Void

    var body: some View {
        if preparation.inProgress {
            ThreadActivityHeader(summary: preparation.text, canCollapse: false, isInProgress: true, expanded: false)
                .accessibilityIdentifier("task.preparation")
        } else {
            VStack(alignment: .leading, spacing: 8) {
                AppText(preparation.text).font(.caption).foregroundStyle(.secondary)
                if let retry = preparation.retry {
                    Button("再試行") { perform(.retryPreparation(retry)) }
                }
            }.accessibilityIdentifier("task.preparation")
        }
    }
}

func conversationRows(_ rows: [ThreadConversationRow],
                      expansion: [String: String]) -> [ThreadConversationRow] {
    var expanded = false
    return rows.filter { row in
        guard case let .native(native, _) = row.content else { return true }
        switch native.content {
        case let .activityHeader(activity):
            expanded = AgentCore.activityIsExpanded(status: activity.status, expandedStatus: expansion[activity.id])
            return true
        case .activity: return expanded
        // Requests live in the composer's dock, rather than in the history.
        case .inProgress, .pendingRequest: return false
        default: return true
        }
    }
}
