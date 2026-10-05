import AgentCore
import SwiftUI

struct ThreadScreen: View {
    @ObservedObject var model: BexAppViewModel
    let openTools: (WorkspaceTab?, Bool) -> Void
    @State private var showingQueue = false
    @State private var renaming = false
    @State private var title = ""
    @State private var nearBottom = true
    @State private var firstVisible: String?
    @State private var initialScroll = true

    var body: some View {
        let conversation = model.conversation
        VStack(spacing: 0) {
            if let notice = model.notice {
                BexNotice(text: notice).font(T3.font(12)).padding(.horizontal, 20).padding(
                    .vertical,
                    6
                )
            }
            ScrollViewReader { reader in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 14) {
                        if conversation.hasMoreHistory {
                            Button("Load earlier messages") { model.loadOlderHistory() }.font(T3.font(13))
                                .frame(maxWidth: .infinity)
                        }
                        if conversation.loading, conversation.rows.isEmpty {
                            ProgressView().frame(maxWidth: .infinity)
                        }
                        ForEach(conversation.rows, id: \.id) { row in
                            ConversationRow(model: model, row: row).id(row.id)
                        }
                        Color.clear.frame(height: 1).id("conversation-bottom")
                    }
                    .scrollTargetLayout().padding(.horizontal, 20).padding(.top, 18).padding(.bottom, 20)
                    .frame(maxWidth: 736).frame(maxWidth: .infinity)
                }
                .scrollPosition(id: $firstVisible, anchor: .top)
                .onScrollGeometryChange(for: Bool.self) { geometry in
                    geometry.contentSize.height - geometry.contentOffset.y - geometry.containerSize.height < 80
                } action: { _, bottom in nearBottom = bottom }
                .onChange(of: conversation.rows) { old, next in
                    if old.first?.id != next.first?.id && old.last?.id == next.last?.id && next.count > old.count {
                        if let first = old.first?.id {
                            reader.scrollTo(first, anchor: .top)
                        }
                    } else if nearBottom || initialScroll {
                        reader.scrollTo("conversation-bottom", anchor: .bottom)
                        if !next.isEmpty {
                            initialScroll = false
                        }
                    }
                }
                .onChange(of: conversation.threadId) { _, _ in initialScroll = true; reader.scrollTo(
                    "conversation-bottom",
                    anchor: .bottom
                ) }
                .onAppear { reader.scrollTo("conversation-bottom", anchor: .bottom) }
            }
            ConversationComposer(model: model, showQueue: { showingQueue = true })
        }
        .background(T3.color("canvas")).foregroundStyle(T3.color("text"))
        .navigationTitle(conversation.title).navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 2) {
                    Text(conversation.project).font(T3.font(11)).foregroundStyle(T3.color("textMuted"))
                    Text(conversation.title).font(T3.font(14, weight: .medium)).lineLimit(1)
                }.onTapGesture { title = conversation.title; renaming = conversation.threadId != nil }
            }
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    if let id = conversation.threadId {
                        if conversation.canMergeBack { Button("Merge back to source") { model.perform(.mergeBack) } }
                        ThreadActions(
                            model: model,
                            id: id,
                            pinned: conversation.pinned,
                            archived: conversation.archived,
                            settled: conversation.settled
                        )
                        if conversation.snoozed {
                            Button("Unsnooze") { model.perform(.thread(
                                threadId: id,
                                action: .unsnooze
                            )) }
                        }
                        Toggle(
                            "Auto-settle",
                            isOn: Binding(
                                get: { conversation.autoSettle },
                                set: { model.perform(.thread(threadId: id, action: .autoSettle(enabled: $0))) }
                            )
                        )
                        Button("Rename") { title = conversation.title; renaming = true }
                    }
                    Divider()
                    Button("Terminal") { openTools(.terminal, false) }
                    Button("Files") { openTools(.files, false) }
                    Button("Diff") { openTools(.files, true) }
                    Button("Browser") { openTools(.browser, false) }
                } label: { Image(systemName: "ellipsis") }
            }
        }
        .sheet(isPresented: $showingQueue) { QueueSheet(model: model) }
        .alert("Rename thread", isPresented: $renaming) {
            TextField("Title", text: $title)
            Button("Save") {
                if let id = conversation.threadId {
                    model.perform(.thread(
                        threadId: id,
                        action: .rename(title: title)
                    ))
                }
            }
            Button("Cancel", role: .cancel) {}
        }
    }
}
