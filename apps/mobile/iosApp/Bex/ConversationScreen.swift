import AgentCore
import PhotosUI
import SwiftUI
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
    @State var isVisible = false
    @State var isFollowingLatest = true
    @State var historyTopVisibility: HistoryTopVisibility?
    @State var latestHistoryRowVisible = false
    @State var expandedItemIds = Set<String>()
    @State var activityExpansionOverrides = [String: ActivityExpansion]()
    @FocusState var composerFocused: Bool

    var body: some View {
        chatContent
            .onDisappear {
                isVisible = false
                dictation.cancel()
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
                isVisible = true
                if model.isNewThread {
                    composerFocused = true
                }
                loadVisibleHistory()
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
        ScrollViewReader { proxy in
            VStack(spacing: 0) {
                if let notice = model.notice {
                    BexNotice(text: notice).padding(.horizontal).padding(.top, 8)
                }
                if let thread = conversation {
                    let rows = conversationRows(thread.rows, expansion: activityExpansionOverrides)
                    let lastRowId = rows.last?.id
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 12) {
                            ForEach(rows) { row in
                                conversationRow(row)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .id(row.id)
                            }
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 6)
                        .background(ConversationScrollToTop {
                            isFollowingLatest = false
                        })
                    }
                    .defaultScrollAnchor(.bottom, for: .initialOffset)
                    .defaultScrollAnchor(.bottom, for: .alignment)
                    // Tail updates preserve a detached reader; prepends retain native offset correction.
                    .transaction(value: lastRowId) { transaction in
                        if !isFollowingLatest {
                            transaction.scrollContentOffsetAdjustmentBehavior = .disabled
                        }
                    }
                    .accessibilityIdentifier("task.detail")
                    .accessibilityValue(threadAccessibilityValue(thread))
                    .buttonStyle(.plain)
                    .simultaneousGesture(DragGesture(minimumDistance: 1).onChanged { gesture in
                        guard abs(gesture.translation.height) > abs(gesture.translation.width) else { return }
                        if isFollowingLatest {
                            isFollowingLatest = false
                        }
                        loadVisibleHistory()
                    }.onEnded { gesture in
                        if gesture.translation.height < -abs(gesture.translation.width), latestHistoryRowVisible {
                            isFollowingLatest = true
                            followLatest(to: lastRowId, using: proxy)
                        }
                    })
                    .onScrollGeometryChange(for: ConversationScrollMetrics.self) { geometry in
                        ConversationScrollMetrics(
                            content: geometry.contentSize,
                            container: geometry.containerSize,
                            oldestVisible: geometry.containerSize.height > 0 && geometry.contentSize.height > 0 &&
                                geometry.visibleRect.minY <= 0,
                            latestVisible: geometry.containerSize.height > 0 && geometry.contentSize.height > 0 &&
                                geometry.visibleRect.minY < geometry.contentSize.height &&
                                geometry.visibleRect.maxY > 0 &&
                                geometry.contentSize.height - geometry.visibleRect.maxY <= 80
                        )
                    } action: { old, new in
                        let top = HistoryTopVisibility(threadId: thread.id, firstRowId: thread.rows.first?.id,
                                                       visible: new.oldestVisible)
                        if historyTopVisibility != top {
                            historyTopVisibility = top
                        }
                        if latestHistoryRowVisible != new.latestVisible {
                            latestHistoryRowVisible = new.latestVisible
                        }
                        // Lazy rows settle after insertion; sending also resizes the keyboard and composer.
                        if old.content != new.content || old.container != new.container || !new.latestVisible {
                            followLatest(to: lastRowId, using: proxy)
                        }
                        loadVisibleHistory()
                    }
                    .onChange(of: thread.id, initial: true) {
                        expandedItemIds.removeAll()
                        activityExpansionOverrides.removeAll()
                        isFollowingLatest = true
                        followLatest(to: lastRowId, using: proxy)
                    }
                    .onChange(of: lastRowId) { followLatest(to: lastRowId, using: proxy) }
                    .overlay(alignment: .bottom) {
                        if !latestHistoryRowVisible {
                            Button {
                                isFollowingLatest = true
                                withAnimation { followLatest(to: lastRowId, using: proxy) }
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
                    .onChange(of: model.loadingHistory) {
                        if !$0 {
                            loadVisibleHistory()
                        }
                    }
                } else if model.isNewThread {
                    GeometryReader { geometry in
                        ScrollView {
                            newThreadContext
                                .padding(24)
                                .frame(maxWidth: .infinity)
                                .frame(minHeight: geometry.size.height)
                        }
                    }
                    .clipped()
                    // The native scroll frame extends under safe-area insets; group the visible viewport.
                    .accessibilityElement(children: .contain)
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
                            if let thread = conversation,
                               let first = conversationRows(thread.rows, expansion: activityExpansionOverrides).first {
                                isFollowingLatest = false
                                withAnimation { proxy.scrollTo(first.id, anchor: .top) }
                            }
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
