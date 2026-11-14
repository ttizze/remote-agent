import AgentCore
import SwiftUI
import UIKit

extension ConversationModel {
    func download(_ path: String) async throws -> URL {
        try await inTemporaryDirectory { directory in
            let target = directory.appendingPathComponent(URL(fileURLWithPath: path).lastPathComponent)
            _ = try await outcome(for: .downloadFile(DownloadFile(source: path, destination: target.path)))
            try Task.checkCancellation()
            return target
        }
    }

    func outcome(for intent: Intent) async throws -> Outcome {
        try await withCheckedThrowingContinuation { continuation in
            perform(intent) { continuation.resume(with: $0) }
        }
    }

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
                        Text(verbatim: control.loadLabel).fixedSize()
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
        case let .historyNotice(message): Text(verbatim: message).font(.caption).foregroundStyle(.secondary)
        case let .native(row, item): nativeConversationRow(row, item: item)
        }
    }

    @ViewBuilder
    func nativeConversationRow(_ projected: VisibleConversationRow, item: ConversationItem?) -> some View {
        let row = projected.data()
        switch (row.content, item) {
        case let (.preparation(preparation), _):
            SetupProgressCard(preparation: preparation, openTerminal: {
                model.openSetupTerminal($0)
                openTools?(nil)
            }, perform: { model.perform($0) })
        case let (.providerHandoff(text), _):
            Text(text).font(.caption).foregroundStyle(.secondary).padding(.vertical, 8)
        case let (.notification(reports, _), _):
            NotificationRow(reports: reports)
        case let (.user, item?):
            userMessageRow(item, row: projected, timestampMs: row.timestampMs)
                .padding(.top, projected.origin() == .queued ? 0 : 16)
        case (let .response(_, showActions, turnId), let item?):
            ThreadMessageRow(
                row: projected,
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
            ).equatable()
        case let (.activityHeader(activity), _): activityHeader(
                activity,
                id: projected.id(),
                expanded: projected.expanded()
            )
        case (let .activity(_, turnId), let item?): activityItem(item, turnId: turnId, rowId: projected.id())
        case let (.error(error), _): ThreadErrorRow(error: error)
        case (.inProgress, _), (.pendingRequest, _), (_, nil): EmptyView()
        }
    }

    func userMessageRow(_ item: ConversationItem, row: VisibleConversationRow, timestampMs: UInt64?) -> some View {
        let thread = model.selectedThreadId
        return ThreadMessageRow(
            row: row,
            item: item,
            isUser: true,
            media: mediaAccess,
            selection: selectionActions,
            timestampMs: timestampMs,
            restoreUnknown: { model.restoreUnknownSubmission($0) },
            discardUnknown: { model.discardUnknownSubmission($0) },
            edit: row.data().editTurnId.map { turn in {
                if let thread {
                    editRequest = ConversationEditRequest(
                        thread: thread,
                        turn: turn
                    )
                }
            } },
            editingEnabled: model.composerControls.runningTurn == nil && !model.conversationContext.rewinding()
        ).equatable()
    }

    @ViewBuilder
    func activityItem(_ item: ConversationItem, turnId: String, rowId: String) -> some View {
        let values = model.activityDecorations[rowId] ?? []
        let request = conversation?.id.map { ReadItem(
            threadId: $0,
            turnId: turnId,
            itemId: item.data.nativeId ?? item.data.id
        ) }
        if !values.isEmpty {
            ThreadDecoratedActivity(
                values: values,
                blocks: item.markdown,
                media: mediaAccess,
                selection: selectionActions,
                canSave: model.isConnected && !model.cwd.isEmpty,
                open: model.openThread,
                plan: { destination, filename, markdown in
                    planRequest = ConversationPlanRequest(
                        destination: destination,
                        cwd: model.cwd,
                        filename: filename,
                        markdown: markdown
                    )
                }
            )
        } else {
            ThreadItemRow(
                item: item,
                media: mediaAccess,
                selection: selectionActions,
                isExpanded: expandedItemIds.contains(item.data.id),
                detail: request.map { model.conversationContext.itemDetail(item: $0) } ?? ItemDetail(
                    loading: false,
                    error: nil
                ),
                toggleExpanded: {
                    scroll.following = false
                    expandedItemIds.formSymmetricDifference([item.data.id])
                },
                loadDetails: {
                    guard let request else { return nil }
                    return await model.readItemDetails(
                        threadId: request.threadId, turnId: request.turnId, itemId: request.itemId
                    )
                },
                watchOutput: { watching in
                    guard item.data.kind == "command", let request,
                          let itemId = item.data.nativeId else { return }
                    model.perform(.watchCommandOutput(threadId: request.threadId, itemId: itemId, watching: watching))
                }
            ).equatable()
        }
    }

    @ViewBuilder
    func activityHeader(_ turn: ActivityPresentation, id: String, expanded: Bool) -> some View {
        let header = ConversationActivityHeader(summary: turn.activitySummary, canCollapse: turn.activityCanCollapse,
                                                isInProgress: turn.isInProgress, expanded: expanded)
        if turn.activityCanCollapse {
            Button {
                scroll.following = false
                model.toggleActivity(id)
            } label: {
                header
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("turn.activity." + turn.id)
        } else {
            header.accessibilityIdentifier("turn.activity." + turn.id)
        }
    }

    func threadAccessibilityValue(_ thread: ConversationPresentation, itemCount: UInt32) -> String {
        "turns=\(thread.projection.source().turnCount());items=\(itemCount)"
    }
}

/// What the background work the agent woke for did, in the app's language.
private struct NotificationRow: View {
    let reports: [BackgroundReport]
    @Environment(\.locale) private var locale

    var body: some View {
        Text(notificationText(reports: reports, language: locale.appLanguage))
            .font(.caption).foregroundStyle(.secondary).padding(.vertical, 8)
    }
}
