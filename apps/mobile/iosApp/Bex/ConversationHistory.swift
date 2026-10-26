import AgentCore
import SwiftUI
import UIKit

/// History and row presentation
extension ThreadScreen {
    func historyControl(_ control: HistoryControl, session: SessionRef, turnCount: UInt64) -> some View {
        VStack(spacing: 6) {
            if control.hasMore {
                Button {
                    let controller = scrollController
                    let request = controller.captureReadingPosition(
                        turnCount: turnCount,
                        hasMoreHistory: control.hasMore
                    )
                    isFollowingLatest = false
                    model.loadOlderHistory(session) { succeeded, turns, hasMore in
                        if let point = controller.historyLoaded(
                            request,
                            succeeded: succeeded,
                            turnCount: turns,
                            hasMoreHistory: hasMore
                        ) {
                            scrollPosition.scrollTo(point: point)
                        }
                    }
                } label: {
                    HStack(spacing: 12) {
                        Rectangle().frame(height: 1).foregroundStyle(Color(UIColor.separator))
                        Text(control.loadLabel).fixedSize()
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
                Text(error).font(.caption).foregroundStyle(.secondary)
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
        case let .historyNotice(message): Text(message).font(.caption).foregroundStyle(.secondary)
        case let .native(content, item): nativeConversationRow(content, item: item)
        case let .queued(item):
            userMessageRow(item)
        }
    }

    @ViewBuilder
    func nativeConversationRow(_ row: ConversationRow, item: ConversationItem?) -> some View {
        switch (row.content, item) {
        case let (.providerHandoff(text), _):
            Text(text).font(.caption).foregroundStyle(.secondary).padding(.vertical, 8)
        case let (.user, item?):
            userMessageRow(item, timestampMs: row.timestampMs)
                .padding(
                    .top,
                    16
                )
        case (let .response(_, turnId), let item?):
            ThreadMessageRow(
                item: item,
                isUser: false,
                media: model.mediaAccess,
                selection: selectionActions,
                timestampMs: row.timestampMs,
                fork: turnId
                    .map { id in { model.forkAndOpen(through: id) { _ in } } },
                forking: turnId
                    .flatMap { turn in model.selectedThreadId.map { model.snapshot.operationRunning(key: .fork(
                        session: $0,
                        turn: turn
                    )) } } ?? false,
                forkError: turnId
                    .flatMap { turn in model.selectedThreadId.flatMap { model.snapshot.operationError(key: .fork(
                        session: $0,
                        turn: turn
                    )) } }
            ).equatable()
        case let (.activityHeader(activity), _): activityHeader(activity)
        case (let .activity(_, turnId), let item?): activityItem(item, turnId: turnId)
        case let (.pendingRequest(request), _): ThreadRequestRow(request: request) { answer, complete in model.respond(
                request,
                answer: answer,
                completion: complete
            ) }
        case let (.error(error), _): ThreadErrorRow(error: error)
        case (.inProgress, _), (_, nil): EmptyView()
        }
    }

    func userMessageRow(_ item: ConversationItem, timestampMs: UInt64? = nil) -> some View {
        ThreadMessageRow(
            item: item,
            isUser: true,
            media: model.mediaAccess,
            selection: selectionActions,
            timestampMs: timestampMs,
            restoreUnknown: { model.restoreUnknownSubmission($0) },
            discardUnknown: { model.discardUnknownSubmission($0) }
        ).equatable()
    }

    func activityItem(_ item: ConversationItem, turnId: String) -> some View {
        ThreadItemRow(
            item: item,
            media: model.mediaAccess,
            selection: selectionActions,
            isExpanded: expandedItemIds.contains(item.data.id),
            toggleExpanded: {
                isFollowingLatest = false
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
        let expanded = AgentCore.activityIsExpanded(activity: turn, choice: activityExpansionOverrides[turn.id])
        if turn.activityCanCollapse {
            Button {
                isFollowingLatest = false
                activityExpansionOverrides[turn.id] = ActivityExpansion(status: turn.status, expanded: !expanded)
                if !expanded, let params = turn.loadItems {
                    model.perform(.loadTurnItems(params))
                }
            } label: {
                ThreadActivityHeader(turn: turn, expanded: expanded)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("turn.activity." + turn.id)
        } else {
            ThreadActivityHeader(turn: turn, expanded: expanded).accessibilityIdentifier("turn.activity." + turn.id)
        }
    }

    func latestBoundaryMarker(_ boundary: LatestBoundary) -> some View {
        Color.clear.frame(height: 80)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
            .id(boundary)
            .onScrollVisibilityChange(threshold: 0.01) { visible in
                if visible {
                    visibleLatest = boundary
                } else if visibleLatest == boundary {
                    visibleLatest = nil
                }
            }
            .onDisappear {
                if visibleLatest == boundary {
                    visibleLatest = nil
                }
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

struct LatestBoundary: Hashable {
    let threadId: SessionRef?
    let rowId: String
}

func conversationRows(_ rows: [ThreadConversationRow],
                      expansion: [String: ActivityExpansion]) -> [ThreadConversationRow] {
    var expanded = false
    return rows.filter { row in
        guard case let .native(native, _) = row.content else { return true }
        switch native.content {
        case let .activityHeader(activity):
            expanded = AgentCore.activityIsExpanded(activity: activity, choice: expansion[activity.id])
            return true
        case .activity: return expanded
        case .inProgress: return false
        default: return true
        }
    }
}
