import AgentCore
import AVFoundation
import SwiftUI
import UniformTypeIdentifiers

struct ThreadScreen: View {
    @ObservedObject var model: BexAppViewModel
    let conversation: RenderedConversation?
    @StateObject var dictation = DictationRecorder()
    @State var sendRecordedText = false
    @State var importing = false
    @State var showingPhotos = false
    @State var showingCamera = false
    @State var preparingMedia = false
    @State var showingFiles = false
    @State var showingModelSettings = false
    @State var scrollViewportHeight: CGFloat = 0
    @State var isFollowingLatest = true
    @State private var isNearLatest = true
    @State var scrollingToOlder = false
    @State var historyBoundaries = [String: CGFloat]()
    @State var historyRequestPending = false
    @State var expandedItemIds = Set<String>()
    @State var activityExpansionOverrides = [String: (status: String, expanded: Bool)]()
    @State var opensDiff = false
    @FocusState var composerFocused: Bool

    private var project: Project? {
        let directory = model.selectedDirectory
        return (model.list?.projects ?? []).first { $0.roots.contains { $0.path == directory } }
    }

    var body: some View {
        VStack(spacing: 0) {
            if let notice = model.notice {
                BexNotice(text: notice).padding(.horizontal).padding(.top, 8)
            }
            if let thread = conversation {
                let rows = model.conversationRows
                let queued = thread.queued()
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 12) {
                            if thread.source().historyCursor() != nil {
                                historyBoundary(nil)
                            }
                            ForEach(rows.indices, id: \.self) { index in
                                let turn = rows[index]
                                conversationTurn(turn, endsNativeTurn: index + 1 == rows.count
                                    || rows[index + 1].turnId != turn.turnId).id(turn.id)
                            }
                            ForEach(queued.indices, id: \.self) { index in
                                let item = queued[index]
                                VStack(alignment: .leading, spacing: 6) {
                                    Text("順番待ち").font(.caption).foregroundColor(.secondary)
                                    ThreadMessageRow(item: item, isUser: true, model: model)
                                }.id(item.id())
                            }
                            Color.clear.frame(height: 1).id("conversation-bottom")
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 6)
                        .background(GeometryReader { geometry in
                            Color.clear.preference(key: LatestMessageBottomPreferenceKey.self,
                                                   value: geometry.frame(in: .named("thread-scroll")).maxY)
                        })
                    }
                    .accessibilityIdentifier("task.detail")
                    .accessibilityValue(threadAccessibilityValue(thread))
                    .buttonStyle(.plain)
                    .simultaneousGesture(DragGesture(minimumDistance: 1).onChanged { gesture in
                        guard abs(gesture.translation.height) > abs(gesture.translation.width) else { return }
                        isFollowingLatest = false
                        scrollingToOlder = gesture.translation.height > 0
                        loadVisibleHistory()
                    }.onEnded { gesture in
                        if gesture.translation.height <= 0, isNearLatest {
                            isFollowingLatest = true
                        }
                    })
                    .onPreferenceChange(LatestMessageBottomPreferenceKey.self) { bottom in
                        isNearLatest = bottom > 0 && bottom <= scrollViewportHeight + 80
                        if isFollowingLatest, bottom > scrollViewportHeight + 1 {
                            proxy.scrollTo("conversation-bottom", anchor: .bottom)
                        }
                    }
                    .overlay(alignment: .bottom) {
                        if !isNearLatest {
                            Button {
                                isFollowingLatest = true
                                scrollingToOlder = false
                                withAnimation { proxy.scrollTo("conversation-bottom", anchor: .bottom) }
                            } label: {
                                Image(systemName: "arrow.down").font(.title3.weight(.medium))
                            }
                            .buttonStyle(.bordered)
                            .buttonBorderShape(.capsule)
                            .controlSize(.large)
                            .accessibilityLabel("最新のメッセージへ")
                            .accessibilityIdentifier("task.latest")
                            .padding(.bottom, 6)
                        }
                    }
                    .coordinateSpace(name: "thread-scroll")
                    .background(GeometryReader { geometry in
                        Color.clear.preference(key: ScrollViewportPreferenceKey.self, value: geometry.size.height)
                    })
                    .onPreferenceChange(HistoryBoundaryPreferenceKey.self) { boundaries in
                        historyBoundaries = boundaries
                        loadVisibleHistory()
                    }
                    .onChange(of: model.loadingHistory) {
                        if !$0 {
                            historyRequestPending = false
                        }
                    }
                    .onPreferenceChange(ScrollViewportPreferenceKey.self) { height in
                        scrollViewportHeight = height
                        if isFollowingLatest {
                            proxy.scrollTo("conversation-bottom", anchor: .bottom)
                        }
                    }
                    .onChange(of: thread.source().id()) { _ in
                        scrollingToOlder = false
                        historyRequestPending = false
                        historyBoundaries.removeAll()
                        expandedItemIds.removeAll()
                        activityExpansionOverrides.removeAll()
                        isFollowingLatest = true
                        proxy.scrollTo("conversation-bottom", anchor: .bottom)
                    }
                }
            } else if model.isNewThread {
                Color.clear.frame(maxWidth: .infinity, maxHeight: .infinity)
                    .accessibilityIdentifier("task.empty")
            } else {
                ProgressView("タスクを読み込み中…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Color(UIColor.systemBackground))
        .safeAreaInset(edge: .bottom, spacing: 0) { composer }
        .onDisappear { dictation.cancel() }
        .onChange(of: model.draftKey) { _ in dictation.cancel() }
        .onChange(of: model.isConnected) {
            if !$0 {
                dictation.cancel()
            }
        }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.item]) { result in
            switch result {
            case let .success(url): model.attach(url)
            case let .failure(error): model.transferError = error.localizedDescription
            }
        }
        .sheet(isPresented: $showingPhotos) {
            ChatPhotoPicker { providers in
                showingPhotos = false
                guard !providers.isEmpty else { return }
                preparingMedia = true
                model.transferError = nil
                importPhotos(providers[...], draftKey: model.draftKey)
            }
        }
        .fullScreenCover(isPresented: $showingCamera) {
            ChatCameraPicker { result in
                showingCamera = false
                finishMediaImport(result)
            }.ignoresSafeArea()
        }
        .sheet(
            isPresented: $showingFiles,
            onDismiss: {
                if !model.selectedDirectory.isEmpty {
                    model.perform(.reviewWorkspace(ReviewWorkspace(cwd: model.cwd)))
                }
            },
            content: {
                WorkspaceSheet(model: model, root: model.cwd, opensDiff: opensDiff)
            }
        )
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
            ToolbarItem(placement: .principal) {
                if !model.isNewThread {
                    conversationTitle
                }
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                if !model.isNewThread {
                    conversationActions
                }
            }
        }
    }
}

/// Conversation navigation
extension ThreadScreen {
    var conversationTitle: some View {
        let directory = model.selectedDirectory
        return VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(conversation?.source().title() ?? (model.isNewThread ? "チャット" : "タスク"))
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
                Button { opensDiff = false; showingFiles = true } label: {
                    Label("ファイル", systemImage: "folder")
                }.accessibilityIdentifier("task.files")
                Button { opensDiff = true; showingFiles = true } label: {
                    Label("変更を表示", systemImage: "plus.forwardslash.minus")
                }
                Button {
                    if let id = conversation?.source().id() {
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
                ForEach((model.list?.projects ?? []), id: \.id) { project in
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
