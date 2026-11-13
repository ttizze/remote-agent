import AgentCore
import SwiftUI
import UIKit

struct ConversationEnvironment {
    let profiles: [HostProfile]
    let hostName: String?
    let list: ThreadList?
    let connecting: Bool
    var selectHost: ((String) -> Void)?
}

struct ThreadScreen: View {
    @Environment(\.locale) private var locale
    let model: ConversationModel
    let environment: ConversationEnvironment
    #if BEX_NSH_CONVERSATION
        @State private var selectedText = ""
    #else
        var conversation: ConversationPresentation? {
            model.conversation
        }
    #endif
    var isSideChat = false
    var openTools: ((PanelKind?) -> Void)?
    @State var openedDirectory: WorkspaceDestination?
    @State var mediaPresentation = ConversationMediaPresentation()
    #if !BEX_NSH_CONVERSATION
        @State var scroll = ConversationScrollState()
        @State var expandedItemIds = Set<String>()
    #endif

    var body: some View {
        chatContent
            .modifier(ConversationMediaPresenter(presentation: mediaPresentation))
            .onDisappear(perform: mediaPresentation.cancelPending)
            .onChange(of: model.draftKey) {
                #if BEX_NSH_CONVERSATION
                    selectedText = ""
                #else
                    expandedItemIds.removeAll()
                    scroll.following = true
                #endif
                openedDirectory = nil
                mediaPresentation.reset()
            }
            .sheet(item: $openedDirectory) { destination in
                ConversationDirectorySheet(model: model, destination: destination) { openedDirectory = nil }
            }
            .navigationTitle(isSideChat ? Text("サイドチャット") : conversationTitleText(
                model.conversationContext.title(), fallback: model.isNewThread ? "新しいタスク" : "タスク"
            ))
            .navigationSubtitle(ifPresent: isSideChat ? nil : conversationSubtitle)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                #if BEX_NSH_CONVERSATION
                    if !selectedText.isEmpty {
                        ToolbarItem(placement: .navigationBarTrailing) {
                            Menu {
                                Button("コピー") { UIPasteboard.general.string = selectedText }
                                Button("チャットに追加") { messageAction("quote", selectedText) }
                                if !isSideChat {
                                    Button("詳細を表示") { messageAction("explain", selectedText) }
                                    Button("サイドチャットで質問") { messageAction("sideChat", selectedText) }
                                }
                            } label: {
                                Label("チャットに追加", systemImage: "text.quote")
                            }
                        }
                    }
                #endif
                if !isSideChat {
                    ToolbarItemGroup(placement: .navigationBarTrailing) {
                        Button {
                            UIApplication.shared.sendAction(
                                #selector(UIResponder.resignFirstResponder),
                                to: nil,
                                from: nil,
                                for: nil
                            )
                            openTools?(nil)
                        } label: {
                            Image(systemName: "square.grid.2x2")
                        }
                        .accessibilityLabel("ツールを開く")
                        .accessibilityIdentifier("task.tools")
                        if !model.isNewThread {
                            conversationActions
                        }
                    }
                }
            }
    }

    private var chatContent: some View {
        let history = model.history
        return VStack(spacing: 0) {
            ErrorNotices(notices: model.conversationContext.notices(), perform: { model.perform($0) })
                .padding(.horizontal)
            #if BEX_NSH_CONVERSATION
                if let error = mediaPresentation.linkFailure {
                    AppNotice(text: error.message).padding(.horizontal)
                }
            #endif
            if let notice = model.notice, errorText(language: locale.appLanguage, error: notice) != history?.error {
                AppNotice(text: notice).padding(.horizontal).padding(.top, 8)
            }
            if model.hasConversation {
                conversationBody
            } else if model.isNewThread {
                ScrollView {
                    newThreadContext
                        .padding(.horizontal, 24)
                        .frame(maxWidth: .infinity)
                }
                .defaultScrollAnchor(.center, for: .alignment)
                .padding(.vertical, 24)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityIdentifier("task.empty")
            } else if let id = model.selectedThreadId, let error = history?.error {
                AppNotice(text: error).padding(.horizontal)
                Button("再試行") { model.openThread(id) }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .accessibilityIdentifier("task.retry")
            } else {
                ProgressView("タスクを読み込み中…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .accessibilityIdentifier("task.loading")
            }
        }
        .background(Color(UIColor.systemBackground))
        .safeAreaInset(edge: .bottom, spacing: 0) {
            ConversationComposer(model: model, mediaPresentation: mediaPresentation,
                                 isSideChat: isSideChat, openTools: openTools)
        }
    }

    @ViewBuilder private var conversationBody: some View {
        #if BEX_NSH_CONVERSATION
            GeometryReader { viewport in
                GeometryReader { canvas in
                    NshConversationView(host: model.host, session: model.session, pane: model.pane,
                                        composerInset: max(0, canvas.size.height - viewport.size.height),
                                        perform: messageAction, preview: mediaAccess.presentation.openPreview)
                        .id(ObjectIdentifier(model))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier("task.detail")
                }
                .ignoresSafeArea(.container, edges: .bottom)
            }
        #else
            let history = model.history
            if let thread = conversation {
                let rows = thread.rows
                let hasHistory = thread.id != nil && history != nil
                let ids: [ConversationList.RowID] = (hasHistory ? [.history] : []) + rows.map { .message($0.id) }
                let showLatest = !scroll.latestVisible && !rows.isEmpty
                GeometryReader { geometry in
                    ConversationList(ids: ids, scroll: scroll, following: scroll.following,
                                     accessibilityValue: threadAccessibilityValue(
                                         thread,
                                         itemCount: thread.projection.sourceItemCount()
                                     ),
                                     viewportHeight: geometry.size.height) { index in
                        let row = index - (hasHistory ? 1 : 0)
                        return AnyView(Group {
                            if row < 0, let session = thread.id, let history {
                                historyControl(history, session: session)
                            } else if rows.indices.contains(row) {
                                conversationRow(rows[row])
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                        .buttonStyle(.plain)
                        .environment(\.locale, locale))
                    }
                    .ignoresSafeArea(.container, edges: .bottom)
                }
                .id(model.draftKey)
                .overlay(alignment: .bottom) { latestButton(visible: showLatest).padding(.bottom, 6) }
            }
        #endif
    }

    #if !BEX_NSH_CONVERSATION
        @ViewBuilder private func latestButton(visible: Bool) -> some View {
            if visible {
                Button { scroll.following = true } label: {
                    Image(systemName: "arrow.down").font(.title3.weight(.medium))
                        .frame(width: 44, height: 44)
                }
                .buttonStyle(.glass)
                .buttonBorderShape(.circle)
                .accessibilityLabel("最新のメッセージへ")
                .accessibilityIdentifier("task.latest")
            }
        }
    #endif
}

/// Conversation navigation
extension ThreadScreen {
    private var project: ProjectSummary? {
        let directory = model.selectedDirectory
        return environment.list?.projects.first { $0.roots.contains { $0.path == directory } }
    }

    /// The project and environment beneath the title, followed by the connection state while it connects.
    var conversationSubtitle: String {
        let directory = model.selectedDirectory
        return [
            project?.name ?? (directory.isEmpty ? "" : URL(fileURLWithPath: directory).lastPathComponent),
            environment.hostName ?? "Mac",
            environment.connecting ? localizedUiText(language: locale.appLanguage, source: "接続中…") : ""
        ].filter { !$0.isEmpty }.joined(separator: " · ")
    }

    #if BEX_NSH_CONVERSATION
        /// What a message asks for: its selection quoted in the composer or asked
        /// about in a side chat, or a link opened.
        func messageAction(_ action: String, _ text: String) {
            switch action {
            case "selection":
                selectedText = text
            case "error":
                model.notice = text
            case "setupTerminal":
                guard let data = text.data(using: .utf8),
                      let value = try? JSONSerialization.jsonObject(with: data) as? [String: String],
                      let handle = value["handle"], let cwd = value["cwd"] else { return }
                model.openSetupTerminal(SetupTerminal(handle: handle, cwd: cwd))
                openTools?(nil)
            case "quote":
                model.addSelectionToChat(text)
            case "sideChat", "explain":
                guard !isSideChat else { return }
                model.askSelectionInSideChat(text)
                openTools?(.sideChat)
            case "link":
                openMessageLink(text)
            default:
                break
            }
        }

        private func openMessageLink(_ text: String) {
            guard let url = URL(string: text) else { return }
            if url.scheme == "https" || url.scheme == "http" ||
                markdownFileTarget(source: text, cwd: mediaAccess.cwd, surface: .mobile) == nil {
                UIApplication.shared.open(url)
            } else {
                mediaAccess.presentation.openLink(text, resolve: mediaAccess.resolve,
                                                  download: mediaAccess.download,
                                                  openDirectory: mediaAccess.openDirectory,
                                                  openFile: mediaAccess.openFile)
            }
        }

    #endif

    var conversationActions: some View {
        Group {
            Button { model.openNewThread(cwd: model.selectedDirectory) } label: {
                Image(systemName: "square.and.pencil")
            }.accessibilityLabel("新しい会話").accessibilityIdentifier("task.new")
            Menu {
                Button { openTools?(.diff) } label: {
                    Label("変更を表示", systemImage: "plus.forwardslash.minus")
                }
                Button {
                    if let id = model.selectedThreadId {
                        model.openThread(id)
                    }
                } label: { Label("更新", systemImage: "arrow.clockwise") }
            } label: {
                Image(systemName: "ellipsis")
            }
            .accessibilityLabel("その他")
            .accessibilityIdentifier("task.more")
        }
        .buttonStyle(.plain)
    }

    private var newThreadContext: some View {
        let directory = model.selectedDirectory
        let emptyFolderLabel = localizedUiText(language: locale.appLanguage, source: "チャット")
        return VStack(spacing: 24) {
            VStack(spacing: 8) {
                Text("何を作りましょうか？")
                    .accessibilityIdentifier("task.prompt")
                Menu {
                    Picker("フォルダ", selection: Binding(get: { directory }, set: { model.openNewThread(cwd: $0) })) {
                        Label("チャット", systemImage: "bubble.left.and.bubble.right").tag("")
                        ForEach(environment.list?.projects ?? [], id: \.id) { project in
                            ForEach(project.roots, id: \.path) { root in
                                Label(project.roots.count == 1 ? project.name : root.path, systemImage: "folder")
                                    .tag(root.path)
                            }
                        }
                    }
                    .pickerStyle(.inline)
                    .labelsHidden()
                    if environment.list?.hasMoreProjects == true {
                        Button("さらにプロジェクトを読み込む") { model.perform(.expandProjects) }
                    }
                } label: {
                    Text(project?
                        .name ??
                        (directory.isEmpty ? emptyFolderLabel : URL(fileURLWithPath: directory).lastPathComponent))
                        .lineLimit(2)
                        .truncationMode(.middle)
                        .frame(minWidth: 44, minHeight: 44)
                }
                .accessibilityLabel(
                    localizedUiFormat(
                        language: locale.appLanguage,
                        source: "フォルダ: {0}",
                        arguments: [project?.name ?? (directory.isEmpty ? emptyFolderLabel : directory)]
                    )
                )
                .accessibilityIdentifier("task.folder")
            }
            .font(.title2)
            Menu {
                Picker("環境", selection: Binding(get: { model.host }, set: { environment.selectHost?($0) })) {
                    ForEach(environment.profiles, id: \.id) { profile in
                        Text(profile.name).tag(profile.id)
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: "laptopcomputer")
                    Text(environment.hostName ?? localizedUiText(language: locale.appLanguage, source: "環境を選択"))
                        .lineLimit(1).truncationMode(.middle)
                    if environment.connecting {
                        ProgressView().controlSize(.small).tint(.red)
                    }
                }
                .frame(minHeight: 44)
            }
            .font(.subheadline)
            .foregroundColor(.secondary)
            .accessibilityLabel(
                localizedUiFormat(
                    language: locale.appLanguage,
                    source: "環境: {0}",
                    arguments: [environment.hostName ?? localizedUiText(language: locale.appLanguage, source: "未選択")]
                )
            )
            .accessibilityIdentifier("task.environment")
        }
        .multilineTextAlignment(.center)
        .buttonStyle(.plain)
        .frame(maxWidth: .infinity)
        .disabled(model.transferring || model.sending || model.transcribing)
    }
}

private func conversationTitleText(_ title: String?, fallback: LocalizedStringKey) -> Text {
    title.map { Text(verbatim: $0) } ?? Text(fallback)
}

private extension View {
    /// Side chats keep only their title.
    @ViewBuilder func navigationSubtitle(ifPresent subtitle: String?) -> some View {
        if let subtitle {
            navigationSubtitle(Text(verbatim: subtitle))
        } else {
            self
        }
    }
}
