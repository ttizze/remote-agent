import AgentCore
import Observation
import SwiftUI

/// User-initiated presentations outlive recycled rows. Inline loads remain owned by each image or web view.
@MainActor @Observable final class ConversationMediaPresentation {
    var preview: SharedFile?
    var imageError: String?
    private(set) var linkFailure: String?
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
        } failed: { self.linkFailure = $0 }
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
        imageError = nil
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
            .alert("画像を開けません", isPresented: Binding(get: { presentation.imageError != nil }, set: {
                if !$0 {
                    presentation.imageError = nil
                }
            })) {
                Button("OK") { presentation.imageError = nil }
            } message: { ErrorMessage(source: presentation.imageError ?? "") }
    }
}
