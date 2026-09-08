import AVFoundation
import RemoteAgentMobile
import SwiftUI
import UniformTypeIdentifiers

struct ThreadScreen: View {
    let state: IosAppViewState
    @ObservedObject var model: BexAppViewModel
    @ObservedObject var conversation: BexConversationModel
    @StateObject var dictation = DictationRecorder()
    @State var sendRecordedText = false
    @State var importing = false
    @State var showingPhotos = false
    @State var showingCamera = false
    @State var preparingMedia = false
    @State var showingFiles = false
    @State var showingModelSettings = false
    @State var scrollViewportHeight: CGFloat = 0
    @State private var isFollowingLatest = true
    @StateObject var scrollPosition = ConversationScrollPosition()
    @State var scrollingToOlder = false
    @State var historyBoundaries = [String: CGFloat]()
    @State var historyRequestPending = false
    @State var expandedItemIds = Set<String>()
    @State var activityExpansionOverrides = [String: (status: String, expanded: Bool)]()
    @State var opensDiff = false
    @State var review: WorkspaceReviewSummary?
    @FocusState var composerFocused: Bool

    private var reviewVersion: String {
        model.cwd + (conversation.thread?.turns.map { "\($0.id):\($0.status)" }.joined(separator: ",") ?? "")
    }

    private var project: IosProjectView? {
        state.projects.first { $0.roots.contains(model.cwd) }
    }

    var body: some View {
        VStack(spacing: 0) {
            if let notice = state.notice {
                BexNotice(text: notice).padding(.horizontal).padding(.top, 8)
            }
            if let thread = conversation.thread {
                List {
                    ForEach(conversationRows(thread)) { row in
                        conversationRow(row)
                            .taskListRowStyle()
                            .listRowInsets(EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
                    }
                }
                .listStyle(.plain)
                .buttonStyle(.plain)
                .environment(\.defaultMinListRowHeight, 0)
                .background(ConversationScrollViewObserver(position: scrollPosition,
                                                           accessibilityValue: threadAccessibilityValue(thread),
                                                           onFollowingLatest: { isFollowingLatest = $0 },
                                                           onDirection: { upward in
                                                               if scrollingToOlder !=
                                                                   upward {
                                                                   scrollingToOlder = upward
                                                               }
                                                               loadVisibleHistory()
                                                           }))
                .overlay(alignment: .bottom) {
                    if !isFollowingLatest {
                        Button { scrollPosition.scrollToLatest(animated: true) } label: {
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
                    Color.clear.preference(key: ScrollViewportPreferenceKey.self,
                                           value: geometry.size.height - geometry.safeAreaInsets.top - geometry
                                               .safeAreaInsets.bottom)
                })
                .onPreferenceChange(HistoryBoundaryPreferenceKey.self) { boundaries in
                    historyBoundaries = boundaries
                    loadVisibleHistory()
                }
                .onChange(of: state.loadingHistory) {
                    if !$0 {
                        historyRequestPending = false
                    }
                }
                .onPreferenceChange(ScrollViewportPreferenceKey.self) { scrollViewportHeight = $0 }
                .onChange(of: thread.id) { _ in
                    scrollingToOlder = false
                    historyRequestPending = false
                    historyBoundaries.removeAll()
                    expandedItemIds.removeAll()
                    activityExpansionOverrides.removeAll()
                    scrollPosition.scrollToLatest(animated: false)
                }
            } else if state.isNewThread {
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
        .onChange(of: state.isConnected) {
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
        .sheet(isPresented: $showingFiles, onDismiss: refreshReview) {
            WorkspaceSheet(model: model, root: model.cwd, opensDiff: opensDiff)
        }
        .sheet(isPresented: $showingModelSettings) { ModelSettingsSheet(model: model.modelSettings) }
        .onAppear {
            if state.isNewThread {
                composerFocused = true
            }
        }
        .onChange(of: state.isNewThread) {
            if $0 {
                composerFocused = true
            }
        }
        .task(id: reviewVersion) { refreshReview() }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                if !state.isNewThread {
                    conversationTitle
                }
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                if !state.isNewThread {
                    conversationActions
                }
            }
        }
    }
}

/// Conversation navigation
extension ThreadScreen {
    var conversationTitle: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(conversation.thread?.title ?? (state.isNewThread ? "チャット" : "タスク"))
                    .font(.headline).lineLimit(1)
                if state.isConnecting {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel("接続中")
                        .accessibilityIdentifier("connection.progress")
                }
            }
            Text([
                project?.name ?? (model.cwd.isEmpty ? "" : URL(fileURLWithPath: model.cwd).lastPathComponent),
                state.selectedProfileName ?? "Mac"
            ].filter { !$0.isEmpty }.joined(separator: " · "))
                .font(.subheadline).foregroundColor(.secondary).lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    var conversationActions: some View {
        HStack(spacing: 0) {
            Button { model.openNewThread(cwd: model.cwd) } label: {
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
                    if let id = conversation.thread?.id {
                        model.openThread(id)
                    }
                    refreshReview()
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
        VStack(alignment: .leading, spacing: 4) {
            Menu {
                ForEach(state.profiles, id: \.id) { profile in
                    Button { model.openNewThread(on: profile.id) } label: {
                        if profile.id == state.selectedProfileId {
                            Label(profile.name, systemImage: "checkmark")
                        } else {
                            Text(profile.name)
                        }
                    }
                }
            } label: {
                contextLabel(state.selectedProfileName ?? "環境を選択", icon: "laptopcomputer")
                if state.isConnecting {
                    ProgressView().controlSize(.small)
                }
            }
            .accessibilityLabel("環境: \(state.selectedProfileName ?? "未選択")")
            .accessibilityIdentifier("task.environment")
            Menu {
                Button { model.openNewThread(cwd: "") } label: {
                    Label("チャット", systemImage: "bubble.left.and.bubble.right")
                }
                ForEach(state.projects, id: \.id) { project in
                    ForEach(project.roots, id: \.self) { root in
                        Button { model.openNewThread(cwd: root) } label: {
                            Label(project.roots.count == 1 ? project.name : root,
                                  systemImage: root == model.cwd ? "checkmark" : "folder")
                        }
                    }
                }
                if state.hasMoreProjects {
                    Button("さらにプロジェクトを読み込む") { model.expandTaskList(projects: true) }
                }
            } label: {
                contextLabel(
                    project?.name ?? (model.cwd.isEmpty ? "チャット" : URL(fileURLWithPath: model.cwd).lastPathComponent),
                    icon: model.cwd.isEmpty ? "bubble.left.and.bubble.right" : "folder"
                )
            }
            .accessibilityLabel("フォルダ: \(project?.name ?? (model.cwd.isEmpty ? "チャット" : model.cwd))")
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

struct WorkspaceReviewSummary {
    let files: Int
    let additions: Int
    let deletions: Int
}
