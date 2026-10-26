import AgentCore
import PhotosUI
import SwiftUI
import SwiftUIIntrospect
import UniformTypeIdentifiers

struct ThreadScreen: View {
    @ObservedObject var model: BexAppViewModel
    let conversation: ConversationPresentation?
    var isSideChat = false
    var openTools: ((WorkspaceTab?, Bool) -> Void)?
    @StateObject var dictation = DictationRecorder()
    @State var sendRecordedText = false
    @State var importing = false
    @State var showingPhotos = false
    @State var selectedPhotos: [PhotosPickerItem] = []
    @State var showingCamera = false
    @State var preparingMedia = false
    @State var showingModelSettings = false
    @State var isFollowingLatest = true
    @State var scrollController = ConversationScrollObserver()
    @State var scrollPosition = ScrollPosition(idType: String.self, edge: .bottom)
    @State var visibleLatest: LatestBoundary?
    @State var expandedItemIds = Set<String>()
    @State var activityExpansionOverrides = [String: ActivityExpansion]()
    @FocusState var composerFocused: Bool

    var body: some View {
        chatContent
            .onDisappear {
                scrollController.onScrollToTop = {}
                scrollController.detach()
                visibleLatest = nil
                dictation.cancel()
            }
            .onChange(of: isFollowingLatest) { _, following in
                if following {
                    scrollController.cancelHistoryRestoration()
                }
            }
            .onChange(of: conversation?.id) {
                expandedItemIds.removeAll()
                activityExpansionOverrides.removeAll()
                visibleLatest = nil
                scrollController.cancelHistoryRestoration()
                isFollowingLatest = true
                scrollPosition.scrollTo(edge: .bottom)
            }
            .onChange(of: model.composerFocusRequest) { _ in
                if model.isShowingSideChat == isSideChat {
                    composerFocused = true
                }
            }
            .onChange(of: model.draftKey) { _ in dictation.cancel() }
            .onChange(of: model.isConnected) {
                if !$0 {
                    dictation.cancel()
                }
            }
            .fileImporter(isPresented: $importing, allowedContentTypes: [.item]) { result in
                Task {
                    do { try await model.attach(result.get()) } catch {
                        model.transferError = error.localizedDescription
                    }
                }
            }
            .photosPicker(isPresented: $showingPhotos, selection: $selectedPhotos,
                          selectionBehavior: .ordered, matching: .any(of: [.images, .videos]),
                          preferredItemEncoding: .compatible)
            .onChange(of: selectedPhotos) { _, items in
                guard !items.isEmpty else { return }
                selectedPhotos = []
                preparingMedia = true
                model.transferError = nil
                let draftKey = model.draftKey
                Task { await importPhotos(items, draftKey: draftKey) }
            }
            .fullScreenCover(isPresented: $showingCamera) {
                ChatCameraPicker { result in
                    showingCamera = false
                    Task {
                        do {
                            if let url = try result.get() {
                                try await model.attach(url, temporaryDirectory: url.deletingLastPathComponent())
                            }
                        } catch { model.transferError = error.localizedDescription }
                    }
                }.ignoresSafeArea()
            }
            .sheet(isPresented: $showingModelSettings) { ModelSettingsSheet(model: model) }
            .onAppear {
                if model.isNewThread {
                    composerFocused = true
                }
            }
            .onChange(of: model.isNewThread) {
                if $0 {
                    composerFocused = true
                }
            }
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .navigationBarTrailing) {
                    if !isSideChat {
                        HStack(spacing: 0) {
                            Button { composerFocused = false; openTools?(nil, false) } label: {
                                Image(systemName: "square.grid.2x2").frame(width: 44, height: 44)
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
    }

    private var chatContent: some View {
        let history = conversation?.id.flatMap { model.snapshot.historyControl(session: $0) }
        let followLatest = $isFollowingLatest
        return VStack(spacing: 0) {
            if let notice = model.notice, notice != history?.error {
                BexNotice(text: notice).padding(.horizontal).padding(.top, 8)
            }
            if let thread = conversation {
                let rows = conversationRows(thread.rows, expansion: activityExpansionOverrides)
                let lastRowId = rows.last?.id
                let latestBoundary = lastRowId.map { LatestBoundary(threadId: thread.id, rowId: $0) }
                let turnCount = thread.source.turnCount()
                let hasMoreHistory = history?.hasMore ?? false
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        if let session = thread.id, let history {
                            historyControl(history, session: session, turnCount: turnCount)
                        }
                        ForEach(rows) { row in
                            conversationRow(row)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .id(row.id)
                                .overlay(alignment: .bottom) {
                                    if row.id == lastRowId, let latestBoundary {
                                        latestBoundaryMarker(latestBoundary)
                                    }
                                }
                        }
                    }
                    .scrollTargetLayout()
                    .padding(.horizontal, 16)
                    .padding(.vertical, 6)
                }
                // Keep the scroll view's identity off introspection's marker views.
                .accessibilityIdentifier("task.detail")
                .accessibilityValue(threadAccessibilityValue(thread))
                .introspect(.scrollView, on: .iOS(.v26...)) { scroll in
                    scrollController.onScrollToTop = { followLatest.wrappedValue = false }
                    scrollController.attach(to: scroll)
                }
                // Measure loaded rows; native text views draw only within the viewport.
                .scrollPosition($scrollPosition, anchor: .bottom)
                .onScrollGeometryChange(for: ConversationScrollLayout.self) { geometry in
                    ConversationScrollLayout(contentHeight: geometry.contentSize.height,
                                             containerHeight: geometry.containerSize.height,
                                             insets: geometry.contentInsets,
                                             turnCount: turnCount,
                                             hasMoreHistory: hasMoreHistory)
                } action: { _, layout in
                    if let point = scrollController.historyDidLayout(turnCount: layout.turnCount,
                                                                     hasMoreHistory: layout.hasMoreHistory,
                                                                     contentHeight: layout.contentHeight) {
                        scrollPosition.scrollTo(point: point)
                    }
                    if isFollowingLatest {
                        scrollPosition.scrollTo(edge: .bottom)
                    }
                }
                .defaultScrollAnchor(.bottom, for: .alignment)
                // Tail updates preserve a detached reader.
                .transaction(value: lastRowId) { transaction in
                    if !isFollowingLatest {
                        transaction.scrollContentOffsetAdjustmentBehavior = .disabled
                    }
                }
                .buttonStyle(.plain)
                .simultaneousGesture(DragGesture(minimumDistance: 1).onChanged { gesture in
                    guard isFollowingLatest,
                          abs(gesture.translation.height) > abs(gesture.translation.width) else { return }
                    isFollowingLatest = false
                }.onEnded { gesture in
                    if gesture.translation.height < -abs(gesture.translation.width),
                       visibleLatest == latestBoundary {
                        scrollController.cancelHistoryRestoration()
                        isFollowingLatest = true
                        scrollPosition.scrollTo(edge: .bottom)
                    }
                })
                .overlay(alignment: .bottom) {
                    if visibleLatest != latestBoundary, !rows.isEmpty {
                        Button {
                            scrollController.cancelHistoryRestoration()
                            isFollowingLatest = true
                            withAnimation { scrollPosition.scrollTo(edge: .bottom) }
                        } label: {
                            Image(systemName: "arrow.down").font(.title3.weight(.medium))
                                .foregroundStyle(.white)
                                .frame(width: 44, height: 44)
                                .background(Color(white: 0.19), in: Circle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("最新のメッセージへ")
                        .accessibilityIdentifier("task.latest")
                        .padding(.bottom, 6)
                    }
                }
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
            } else if let id = model.selectedThreadId, model.notice != nil {
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
        .safeAreaInset(edge: .bottom, spacing: 0) { composer }
        .toolbar {
            ToolbarItem(placement: .principal) {
                if !isSideChat, !model.isNewThread {
                    Button {
                        scrollController.cancelHistoryRestoration()
                        isFollowingLatest = false
                        withAnimation { scrollPosition.scrollTo(edge: .top) }
                    } label: {
                        conversationTitle
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("会話の先頭へ")
                    .accessibilityIdentifier("task.top")
                }
            }
        }
    }
}

/// Offset-only changes are user scrolling or an in-flight animation, not a new layout.
private struct ConversationScrollLayout: Equatable {
    let contentHeight: CGFloat
    let containerHeight: CGFloat
    let insets: EdgeInsets
    let turnCount: UInt64
    let hasMoreHistory: Bool
}

/// Conversation navigation
extension ThreadScreen {
    private var project: ProjectSummary? {
        let directory = model.selectedDirectory
        return model.list?.projects.first { $0.roots.contains { $0.path == directory } }
    }

    var conversationTitle: some View {
        let directory = model.selectedDirectory
        return VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(conversation?.title ?? (model.isNewThread ? "チャット" : "タスク"))
                    .font(.headline).lineLimit(1)
                if model.isConnecting {
                    ProgressView().controlSize(.small)
                        .tint(.red)
                        .accessibilityLabel("接続中")
                        .accessibilityIdentifier("connection.progress")
                }
            }
            Text([
                project?.name ?? (directory.isEmpty ? "" : URL(fileURLWithPath: directory).lastPathComponent),
                model.selectedProfileName ?? "Mac"
            ].filter { !$0.isEmpty }.joined(separator: " · "))
                .font(.subheadline).foregroundColor(.secondary).lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    var conversationActions: some View {
        HStack(spacing: 0) {
            Button { model.openNewThread(cwd: model.selectedDirectory) } label: {
                Image(systemName: "square.and.pencil").font(.title2).frame(width: 44, height: 44)
            }.accessibilityLabel("新しい会話").accessibilityIdentifier("task.new")
            Menu {
                Button { openTools?(.files, true) } label: {
                    Label("変更を表示", systemImage: "plus.forwardslash.minus")
                }
                Button {
                    if let id = model.selectedThreadId {
                        model.openThread(id)
                    }
                } label: { Label("更新", systemImage: "arrow.clockwise") }
            } label: {
                Image(systemName: "ellipsis").font(.title2.weight(.semibold)).frame(width: 44, height: 44)
            }
            .accessibilityLabel("その他")
            .accessibilityIdentifier("task.more")
        }
        .buttonStyle(.plain)
    }

    private var newThreadContext: some View {
        let directory = model.selectedDirectory
        return VStack(spacing: 24) {
            VStack(spacing: 8) {
                Text("何を作りましょうか？")
                    .accessibilityIdentifier("task.prompt")
                Menu {
                    Button { model.openNewThread(cwd: "") } label: {
                        Label("チャット", systemImage: "bubble.left.and.bubble.right")
                    }
                    ForEach(model.list?.projects ?? [], id: \.id) { project in
                        ForEach(project.roots, id: \.path) { root in
                            Button { model.openNewThread(cwd: root.path) } label: {
                                Label(project.roots.count == 1 ? project.name : root.path,
                                      systemImage: root.path == directory ? "checkmark" : "folder")
                            }
                        }
                    }
                    if model.list?.hasMoreProjects == true {
                        Button("さらにプロジェクトを読み込む") { model.perform(.expandProjects) }
                    }
                } label: {
                    Text(project?
                        .name ?? (directory.isEmpty ? "チャット" : URL(fileURLWithPath: directory).lastPathComponent))
                        .underline()
                        .lineLimit(2)
                        .truncationMode(.middle)
                        .frame(minWidth: 44, minHeight: 44)
                }
                .accessibilityLabel("フォルダ: \(project?.name ?? (directory.isEmpty ? "チャット" : directory))")
                .accessibilityIdentifier("task.folder")
            }
            .font(.largeTitle.weight(.medium))
            Menu {
                ForEach(model.profiles, id: \.id) { profile in
                    Button { model.openNewThread(on: profile.id) } label: {
                        if profile.id == model.selectedProfileId {
                            Label(profile.name, systemImage: "checkmark")
                        } else {
                            Text(profile.name)
                        }
                    }
                }
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: "laptopcomputer")
                    Text(model.selectedProfileName ?? "環境を選択")
                        .lineLimit(1).truncationMode(.middle)
                    if model.isConnecting {
                        ProgressView().controlSize(.small).tint(.red)
                    }
                }
                .frame(minHeight: 44)
            }
            .font(.subheadline)
            .foregroundColor(.secondary)
            .accessibilityLabel("環境: \(model.selectedProfileName ?? "未選択")")
            .accessibilityIdentifier("task.environment")
        }
        .multilineTextAlignment(.center)
        .buttonStyle(.plain)
        .frame(maxWidth: .infinity)
        .disabled(model.transferring || preparingMedia || model.sending || dictation.isRecording || dictation
            .requestingPermission || model.transcribing)
    }
}
