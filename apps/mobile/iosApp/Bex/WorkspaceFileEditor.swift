import AgentCore
import AVKit
import QuickLook
import SwiftUI
import UIKit
import WebKit

private struct WorkspaceFilePreview: View {
    let entry: FileEntry
    let mimeType: String
    let download: @MainActor (String) async throws -> URL
    let aiEdit: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var file: DownloadedFile?
    @State private var error: String?
    @State private var attempt = UUID()

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading) {
                Text(entry.path).font(.caption).foregroundStyle(.secondary)
                    .textSelection(.enabled).padding(.horizontal)
                if let file {
                    preview(file).frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if let error {
                    AppNotice(text: error).padding()
                } else {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                HStack {
                    Button("再読込") { file = nil; attempt = UUID() }
                        .disabled(file == nil && error == nil).accessibilityIdentifier("file.reload")
                    Button("AIで編集") { aiEdit(entry.path) }.accessibilityIdentifier("file.ai-edit")
                }.padding()
            }
            .navigationTitle(entry.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("閉じる") { dismiss() }.accessibilityIdentifier("file.close")
                }
                ToolbarItem(placement: .confirmationAction) {
                    if let url = file?.url {
                        ShareLink(item: url) { Image(systemName: "square.and.arrow.up") }
                            .accessibilityLabel("保存・共有")
                            .accessibilityIdentifier("file.share")
                    }
                }
            }
        }
        .task(id: attempt) {
            guard file == nil else { return }
            error = nil
            do {
                let downloaded = try await DownloadedFile(url: download(entry.path), mimeType: mimeType)
                guard !Task.isCancelled else { return }
                file = downloaded
            } catch {
                if !Task.isCancelled {
                    self.error = error.localizedDescription
                }
            }
        }
    }

    @ViewBuilder private func preview(_ file: DownloadedFile) -> some View {
        if let playback = file.playback {
            switch playback {
            case let .success(player):
                VideoPlayer(player: player).accessibilityIdentifier("file.preview.media")
            case let .failure(error):
                AppNotice(text: error.localizedDescription).padding()
            }
        } else if mimeType == "image/svg+xml" {
            // QuickLook does not draw SVG.
            FileWebPreview(url: file.url, mimeType: mimeType)
        } else if QLPreviewController.canPreview(file.url as NSURL) {
            ConversationFilePreview(url: file.url).accessibilityIdentifier("file.preview")
        } else {
            AppNotice(text: "このファイル形式はプレビューできません").padding()
        }
    }

    /// Native players can temporarily cover the sheet. Retain both the file
    /// and playback resources until the preview itself is released.
    private final class DownloadedFile {
        let url: URL
        let playback: Result<AVPlayer, Error>?
        private let audioConfiguration: AudioSessionConfiguration?

        init(url: URL, mimeType: String) async {
            self.url = url
            if mimeType.hasPrefix("video/") || mimeType.hasPrefix("audio/") {
                do {
                    let asset = AVURLAsset(url: url)
                    guard try await asset.load(.isPlayable) else {
                        throw CocoaError(.fileReadCorruptFile)
                    }
                    try Task.checkCancellation()
                    let session = AVAudioSession.sharedInstance()
                    // Leave an active recording session in charge of its audio configuration.
                    let previous = [AVAudioSession.Category.record, .playAndRecord].contains(session.category)
                        ? nil : AudioSessionConfiguration(
                            category: session.category, mode: session.mode, options: session.categoryOptions
                        )
                    if previous != nil {
                        try session.setCategory(
                            .playback,
                            mode: mimeType.hasPrefix("video/") ? .moviePlayback : .default
                        )
                    }
                    playback = .success(AVPlayer(playerItem: AVPlayerItem(asset: asset)))
                    audioConfiguration = previous
                } catch {
                    playback = .failure(error)
                    audioConfiguration = nil
                }
            } else {
                playback = nil
                audioConfiguration = nil
            }
        }

        deinit {
            if case let .success(player)? = playback {
                player.pause()
            }
            if let audioConfiguration, AVAudioSession.sharedInstance().category == .playback {
                try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
                try? audioConfiguration.restore()
            }
            try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
        }
    }
}

struct WorkspaceFileSheet: View {
    let files: AgentCore.Files
    let paneId: String
    @Binding var text: String
    let perform: SnapshotRequest
    let entry: FileEntry
    let line: UInt32?
    let download: @MainActor (String) async throws -> URL
    let aiEdit: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var error: String?
    @State private var busy = false
    @State private var sharedFile: SharedFile?
    @State private var initialized = false
    @State private var confirmReload = false
    @State private var selection: TextSelection?
    private var controls: FileEditorPresentation {
        files.fileEditor(path: entry.path)
    }

    var body: some View {
        if let mimeType = filePreviewMimeType(path: entry.path) {
            WorkspaceFilePreview(entry: entry, mimeType: mimeType, download: download, aiEdit: aiEdit)
        } else {
            editor
        }
    }

    private var editor: some View {
        NavigationStack {
            VStack(alignment: .leading) {
                Text(entry.path).font(.caption).foregroundColor(.secondary).textSelection(.enabled).padding(.horizontal)
                if let error {
                    AppNotice(text: error).padding(.horizontal).accessibilityIdentifier("file.error")
                }
                if busy {
                    ProgressView().padding()
                }
                if controls.truncated {
                    AppText("file.preview.truncated").font(.caption).padding(.horizontal)
                    FileTextPreview(text: controls.text, line: line)
                } else {
                    BufferedTextInput(value: $text) { TextEditor(text: $0, selection: $selection) }
                        .font(.body.monospaced())
                        .textInputAutocapitalization(.never).disableAutocorrection(true)
                        .accessibilityIdentifier("file.editor")
                        .disabled(!controls.canEdit)
                }
                HStack {
                    Button("再読込") {
                        if controls.modified {
                            confirmReload = true
                        } else {
                            load(restoreDraft: false)
                        }
                    }.disabled(busy)
                    Button("ダウンロード", action: share).disabled(busy).accessibilityIdentifier("file.download")
                    Button("AIで編集") { aiEdit(entry.path) }.accessibilityIdentifier("file.ai-edit")
                }.padding()
            }
            .navigationTitle(entry.name)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("閉じる") { dismiss() }.accessibilityIdentifier("file.close")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("保存") {
                        busy = true; error = nil
                        perform(.saveFile(SaveFile(path: entry.path))) { _, result in
                            busy = false
                            if case let .failure(failure) = result {
                                error = failure.localizedDescription
                            }
                        }
                    }.disabled(busy || !controls.canSave)
                        .accessibilityIdentifier("file.save")
                }
            }
            .onChange(of: controls.loadedRevision) { moveToLine() }
            .onAppear {
                if !initialized {
                    load(restoreDraft: true)
                }
            }
            .confirmationDialog("保存していない編集を破棄して再読込しますか？", isPresented: $confirmReload, titleVisibility: .visible) {
                Button("編集を破棄して再読込", role: .destructive) {
                    load(restoreDraft: false)
                }
            }
            .sheet(item: $sharedFile) { ShareSheet(file: $0) }
        }
    }

    /// Shares the Host's saved file, not the unsaved draft.
    private func share() {
        busy = true; error = nil
        Task {
            defer { busy = false }
            do {
                sharedFile = try await SharedFile(url: download(entry.path))
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    private func moveToLine() {
        guard let line else { return }
        let start = fileLineStart(text, line: line)
        selection = TextSelection(range: start ..< start)
    }

    private func load(restoreDraft: Bool) {
        busy = true; error = nil
        perform(.readFile(ReadFile(path: entry.path, discardDraft: !restoreDraft))) { snapshot, result in
            busy = false
            if case let .failure(failure) = result {
                error = failure.localizedDescription; return
            }
            initialized = true
            if let notice = snapshot.files(paneId: paneId)?.fileEditor(path: entry.path).conflictNotice {
                error = notice
            }
        }
    }
}

/// Scripts, cookies and navigation stay off for a previewed document.
private struct FileWebPreview: View {
    let url: URL
    let mimeType: String
    @State private var page: WebPage?

    var body: some View {
        ZStack {
            if let page {
                WebView(page)
            } else {
                ProgressView()
            }
        }
        .task(id: url) {
            let url = url
            guard let data = try? await Task.detached(priority: .userInitiated, operation: {
                try Data(contentsOf: url)
            }).value, !Task.isCancelled else { return }
            let page = page ?? WebPage(configuration: Self.configuration, navigationDecider: InitialDocumentOnly())
            self.page = page
            page.load(data, mimeType: mimeType, characterEncoding: .utf8, baseURL: URL(string: "about:blank")!)
        }
    }

    private static var configuration: WebPage.Configuration {
        var configuration = WebPage.Configuration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultNavigationPreferences.allowsContentJavaScript = false
        return configuration
    }
}

private func fileLineStart(_ text: String, line: UInt32) -> String.Index {
    String.Index(utf16Offset: Int(fileLineStartUtf16(text: text, line: line)), in: text)
}

private struct FileTextPreview: UIViewRepresentable {
    let text: String
    let line: UInt32?

    func makeUIView(context _: Context) -> UITextView {
        // TextKit 2 can omit very long paragraphs. Lay out the bounded file
        // prefix with TextKit 1, allowing sections to be laid out independently.
        let view = UITextView(usingTextLayoutManager: false)
        view.layoutManager.allowsNonContiguousLayout = true
        view.isEditable = false
        view.isSelectable = true
        view.backgroundColor = .clear
        view.font = .monospacedSystemFont(ofSize: UIFont.preferredFont(forTextStyle: .body).pointSize, weight: .regular)
        view.adjustsFontForContentSizeCategory = true
        view.accessibilityIdentifier = "file.preview.text"
        return view
    }

    func updateUIView(_ view: UITextView, context _: Context) {
        guard view.text != text else { return }
        view.text = text
        if let line {
            let start = fileLineStart(text, line: line)
            view.scrollRangeToVisible(NSRange(start ..< start, in: text))
        }
    }
}

/// Pages stay on the document they were given; links and scripted navigation do not leave it.
struct InitialDocumentOnly: WebPage.NavigationDeciding {
    @MainActor func decidePolicy(for action: WebPage.NavigationAction,
                                 preferences _: inout WebPage.NavigationPreferences) async -> WKNavigationActionPolicy {
        action.request.url?.scheme == "about" && action.navigationType == .other ? .allow : .cancel
    }
}
