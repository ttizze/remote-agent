import AgentCore
import Observation
import SwiftUI

/// User-initiated presentations outlive recycled rows. Inline loads remain owned by each image or web view.
@MainActor @Observable final class ConversationMediaPresentation {
    struct Visualization: Identifiable {
        let id = UUID()
        let html: String
    }

    struct FileFailure {
        let path: String
        let message: String
    }

    var preview: SharedFile?
    var sharedFile: SharedFile?
    var visualization: Visualization?
    var imageError: String?
    private(set) var shareFailure: FileFailure?
    private(set) var linkFailure: FileFailure?
    @ObservationIgnored private var pending: Task<Void, Never>?

    isolated deinit {
        pending?.cancel()
    }

    func openImage(_ data: Data) {
        imageError = nil
        load { try await writeConversationImage(data) } present: {
            self.preview = $0
        } failed: { self.imageError = $0 }
    }

    func share(_ path: String, download: @escaping @MainActor (String) async throws -> URL) {
        shareFailure = nil
        load { try await download(path) } present: {
            self.sharedFile = $0
        } failed: { self.shareFailure = FileFailure(path: path, message: $0) }
    }

    func openLink(_ target: String,
                  resolve: @escaping @MainActor (String) async throws -> ConversationMediaAccess.ResolvedFile,
                  download: @escaping @MainActor (String) async throws -> URL,
                  openDirectory: @escaping @MainActor (String) -> Void,
                  openFile: @escaping @MainActor (String, UInt32?) -> Void) {
        linkFailure = nil
        perform {
            let entry = try await resolve(target)
            try Task.checkCancellation()
            if entry.isDirectory {
                openDirectory(entry.path)
            } else if filePreviewMimeType(path: entry.path)?.hasPrefix("video/") == true {
                let file = try await SharedFile(url: download(entry.path))
                try Task.checkCancellation()
                self.preview = file
            } else {
                openFile(entry.path, entry.line)
            }
        } failed: { self.linkFailure = FileFailure(path: target, message: $0) }
    }

    private func load(_ operation: @escaping @MainActor () async throws -> URL,
                      present: @escaping @MainActor (SharedFile) -> Void,
                      failed: @escaping @MainActor (String) -> Void) {
        perform({
            let file = try await SharedFile(url: operation())
            guard !Task.isCancelled else { return }
            present(file)
        }, failed: failed)
    }

    private func perform(_ operation: @escaping @MainActor () async throws -> Void,
                         failed: @escaping @MainActor (String) -> Void) {
        cancelPending()
        pending = Task {
            defer {
                if !Task.isCancelled {
                    pending = nil
                }
            }
            do {
                try await operation()
            } catch {
                if !Task.isCancelled {
                    failed(error.localizedDescription)
                }
            }
        }
    }

    func cancelPending() {
        pending?.cancel()
        pending = nil
    }

    func reset() {
        cancelPending()
        preview = nil
        sharedFile = nil
        visualization = nil
        imageError = nil
        shareFailure = nil
        linkFailure = nil
    }
}

struct ConversationMediaPresenter: ViewModifier {
    @Bindable var presentation: ConversationMediaPresentation

    func body(content: Content) -> some View {
        content
            .fullScreenCover(item: $presentation.preview) { file in
                ConversationPreview(file: file)
            }
            .sheet(item: $presentation.sharedFile) { ShareSheet(file: $0) }
            .sheet(item: $presentation.visualization) { document in
                NavigationStack {
                    VisualizationWebView(html: document.html)
                        .toolbar {
                            ToolbarItem(placement: .confirmationAction) {
                                Button("閉じる") { presentation.visualization = nil }
                            }
                        }
                }
            }
            .alert("画像を開けません", isPresented: Binding(get: { presentation.imageError != nil }, set: {
                if !$0 {
                    presentation.imageError = nil
                }
            })) {
                Button("OK") { presentation.imageError = nil }
            } message: { AppText(presentation.imageError ?? "") }
    }
}
