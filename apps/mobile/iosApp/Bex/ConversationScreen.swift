import AgentCore
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

struct ConversationViewport: Equatable {
    let oldestVisible: Bool
    let latestVisible: Bool
}

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
    @State var historyViewport = ConversationViewport(oldestVisible: false, latestVisible: false)
    @State private var scrollToTopRequest = 0
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
                ToolbarItem(placement: .principal) {
                    if !isSideChat, !model.isNewThread {
                        Button {
                            scrollToTopRequest += 1
                        } label: {
                            conversationTitle
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("会話の先頭へ")
                        .accessibilityIdentifier("task.top")
                    }
                }
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
        VStack(spacing: 0) {
            if let notice = model.notice {
                BexNotice(text: notice).padding(.horizontal).padding(.top, 8)
            }
            if let thread = conversation {
                let rows = conversationRows(thread, expansion: activityExpansionOverrides)
                let latestRowId = rows.last?.id
                ScrollViewReader { proxy in
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
                    .accessibilityIdentifier("task.detail")
                    .accessibilityValue(threadAccessibilityValue(thread))
                    .buttonStyle(.plain)
                    .simultaneousGesture(DragGesture(minimumDistance: 1).onChanged { gesture in
                        guard abs(gesture.translation.height) > abs(gesture.translation.width) else { return }
                        isFollowingLatest = false
                        loadVisibleHistory()
                    }.onEnded { gesture in
                        if gesture.translation.height <= 0, historyViewport.latestVisible {
                            isFollowingLatest = true
                        }
                    })
                    .onChange(of: scrollToTopRequest) {
                        isFollowingLatest = false
                        if let first = rows.first {
                            withAnimation { proxy.scrollTo(first.id, anchor: .top) }
                        }
                    }
                    .onScrollGeometryChange(for: ConversationViewport.self) { geometry in
                        ConversationViewport(
                            oldestVisible: geometry.containerSize.height > 0 &&
                                geometry.visibleRect.minY < geometry.containerSize.height * 0.6,
                            latestVisible: geometry.contentSize.height - geometry.visibleRect.maxY <= 80
                        )
                    } action: { _, viewport in
                        historyViewport = viewport
                        loadVisibleHistory()
                    }
                    // Offset changes must not request another scroll. Follow only content
                    // growth; lazy row measurement can otherwise keep re-entering layout.
                    .onScrollGeometryChange(for: CGSize.self) { $0.contentSize } action: { _, _ in
                        if isFollowingLatest, let latestRowId {
                            proxy.scrollTo(latestRowId, anchor: .bottom)
                        }
                    }
                    .overlay(alignment: .bottom) {
                        if !historyViewport.latestVisible {
                            Button {
                                isFollowingLatest = true
                                if let latestRowId {
                                    withAnimation { proxy.scrollTo(latestRowId, anchor: .bottom) }
                                }
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
                    .onScrollGeometryChange(for: CGFloat.self) { $0.containerSize.height } action: { _, _ in
                        if isFollowingLatest, let latestRowId {
                            proxy.scrollTo(latestRowId, anchor: .bottom)
                        }
                    }
                    .onChange(of: latestRowId) { id in
                        if isFollowingLatest, let id {
                            proxy.scrollTo(id, anchor: .bottom)
                        }
                    }
                    .onChange(of: thread.id) { _ in
                        expandedItemIds.removeAll()
                        activityExpansionOverrides.removeAll()
                        isFollowingLatest = true
                        if let latestRowId {
                            proxy.scrollTo(latestRowId, anchor: .bottom)
                        }
                    }
                }
            } else if model.isNewThread {
                Color.clear.frame(maxWidth: .infinity, maxHeight: .infinity)
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
    }
}

/// Conversation navigation
extension ThreadScreen {
    private var project: Project? {
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

    var newThreadContext: some View {
        let directory = model.selectedDirectory
        return VStack(alignment: .leading, spacing: 4) {
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
                contextLabel(model.selectedProfileName ?? "環境を選択", icon: "laptopcomputer")
                if model.isConnecting {
                    ProgressView().controlSize(.small)
                }
            }
            .accessibilityLabel("環境: \(model.selectedProfileName ?? "未選択")")
            .accessibilityIdentifier("task.environment")
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
                    Button("さらにプロジェクトを読み込む") { model.expandTaskList(projects: true) }
                }
            } label: {
                contextLabel(
                    project?.name ?? (directory.isEmpty ? "チャット" : URL(fileURLWithPath: directory).lastPathComponent),
                    icon: directory.isEmpty ? "bubble.left.and.bubble.right" : "folder"
                )
            }
            .accessibilityLabel("フォルダ: \(project?.name ?? (directory.isEmpty ? "チャット" : directory))")
            .accessibilityIdentifier("task.folder")
        }
        .font(.title3)
        .foregroundColor(.secondary)
        .buttonStyle(.plain)
        .frame(maxWidth: .infinity, alignment: .leading)
        .disabled(model.transferring || preparingMedia || model.sending || dictation.isRecording || dictation
            .requestingPermission || model.transcribing)
    }

    func contextLabel(_ title: String, icon: String) -> some View {
        HStack(spacing: 12) {
            Image(systemName: icon).frame(width: 28)
            Text(title).lineLimit(1).truncationMode(.middle)
            Image(systemName: "chevron.up.chevron.down").font(.caption.weight(.semibold))
        }
        .frame(minHeight: 44)
        .padding(.horizontal, 8)
    }
}
