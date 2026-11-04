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
        } else if mimeType.hasPrefix("image/") {
            if ["image/svg+xml", "image/gif"].contains(mimeType) {
                FileWebPreview(url: file.url)
            } else {
                ConversationImagePreview(url: file.url)
            }
        } else if QLPreviewController.canPreview(file.url as NSURL) {
            ConversationFilePreview(url: file.url)
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

        init(url: URL, mimeType: String) {
            self.url = url
            if mimeType.hasPrefix("video/") || mimeType.hasPrefix("audio/") {
                let session = AVAudioSession.sharedInstance()
                // An active dictation session already supports playback and
                // retains ownership of the microphone's audio configuration.
                let previous = [AVAudioSession.Category.record, .playAndRecord].contains(session.category)
                    ? nil : AudioSessionConfiguration(
                        category: session.category, mode: session.mode, options: session.categoryOptions
                    )
                let result = Result {
                    if previous != nil {
                        try session.setCategory(
                            .playback,
                            mode: mimeType.hasPrefix("video/") ? .moviePlayback : .default
                        )
                    }
                    return AVPlayer(url: url)
                }
                playback = result
                audioConfiguration = (try? result.get()) == nil ? nil : previous
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
    let snapshot: AgentCore.Snapshot
    @Binding var text: String
    let perform: SnapshotRequest
    let entry: FileEntry
    let line: UInt32?
    let download: @MainActor (String) async throws -> URL
    let share: (String) -> Void
    let aiEdit: (String) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var error: String?
    @State private var busy = false
    @State private var initialized = false
    @State private var confirmReload = false
    @State private var selection: TextSelection?
    private var file: FileContent? {
        snapshot.file().flatMap { $0.path == entry.path ? $0 : nil }
    }

    private var revision: String {
        snapshot.fileDraft(path: entry.path)?.revision ?? file?.revision ?? ""
    }

    private var savedText: String {
        file?.text ?? ""
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
                if let file, file.truncated {
                    AppText(filePreviewTruncationNotice()).font(.caption).padding(.horizontal)
                    FileTextPreview(text: file.text, line: line)
                } else {
                    BufferedTextInput(value: $text) { TextEditor(text: $0, selection: $selection) }
                        .font(.body.monospaced())
                        .textInputAutocapitalization(.never).disableAutocorrection(true)
                        .accessibilityIdentifier("file.editor")
                        .disabled(revision.isEmpty)
                }
                HStack {
                    Button("再読込") {
                        if text != savedText {
                            confirmReload = true
                        } else {
                            load(restoreDraft: false)
                        }
                    }
                    Button("ダウンロード") { share(entry.path) }.accessibilityIdentifier("file.download")
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
                    }.disabled(busy || revision.isEmpty || file?.truncated == true || text == savedText)
                        .accessibilityIdentifier("file.save")
                }
            }
            .onChange(of: file?.revision) { moveToLine() }
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
            if let file = snapshot.file(), file.path == entry.path,
               (snapshot.fileDraft(path: entry.path)?.revision ?? file.revision) != file.revision {
                error = "ホストのファイルが変更されています。下書きは保持しました。再読込すると下書きを破棄します。"
            }
        }
    }
}

private struct FileWebPreview: UIViewRepresentable {
    let url: URL

    func makeUIView(context _: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        return WKWebView(frame: .zero, configuration: configuration)
    }

    func updateUIView(_ view: WKWebView, context _: Context) {
        guard view.url != url else { return }
        view.loadFileURL(url, allowingReadAccessTo: url)
    }
}

private func fileLineStart(_ text: String, line: UInt32) -> String.Index {
    let starts = [text.startIndex] + text.indices.filter { text[$0] == "\n" }.map { text.index(after: $0) }
    return starts[min(max(0, Int(line) - 1), starts.count - 1)]
}

private struct FileTextPreview: UIViewRepresentable {
    let text: String
    let line: UInt32?

    func makeUIView(context _: Context) -> UITextView {
        let view = UITextView()
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
