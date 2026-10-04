import AgentCore
import ImageIO
import Photos
import QuickLook
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Host paths always use the authenticated transfer; never read a Host path from the phone's filesystem.
func conversationFileURL(_ source: String, cwd: String) throws -> URL {
    var source = source
    if let line = source.range(of: ":[0-9]+$", options: .regularExpression) {
        source.removeSubrange(line)
    }
    let base = URL(fileURLWithPath: cwd, isDirectory: true)
    guard let url = URL(string: source, relativeTo: base)?.absoluteURL,
          url.isFileURL,
          (url.host?.isEmpty ?? true) || url.host == "localhost",
          var components = URLComponents(url: url, resolvingAgainstBaseURL: true)
    else {
        throw URLError(.unsupportedURL)
    }
    components.fragment = nil
    components.query = nil
    guard let resolved = components.url else { throw URLError(.badURL) }
    return resolved
}

extension SessionImage {
    init(reference: String) {
        if reference.hasPrefix("data:image/"), let comma = reference.firstIndex(of: ","),
           reference[..<comma].hasSuffix(";base64") {
            self.init(source: String(reference[reference.index(after: comma)...]), encoded: true)
        } else {
            self.init(source: reference, encoded: false)
        }
    }
}

/// Media views use authenticated Host operations.
struct ConversationMediaAccess {
    let host: String?
    let cwd: String
    let download: @MainActor (String) async throws -> URL
    let visualization: @MainActor (String) async throws -> String
}

struct ConversationImage: View {
    let source: SessionImage
    let label: String
    let identifier: String
    let media: ConversationMediaAccess
    var contentMode: ContentMode = .fit
    @State private var image: UIImage?
    @State private var original: Data?
    @State private var previewURL: URL?
    @State private var preparingPreview = false
    @State private var error: String?
    private struct LoadID: Equatable {
        let host: String?
        let source: SessionImage
    }

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image).resizable().aspectRatio(contentMode: contentMode)
                    .frame(maxWidth: .infinity, maxHeight: 420)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                    .accessibilityLabel(label.isEmpty ? "画像" : label)
                    .accessibilityIdentifier(identifier)
                    .onTapGesture {
                        guard !preparingPreview, let original else { return }
                        preparingPreview = true
                        Task {
                            do {
                                previewURL = try await writeConversationImage(original)
                            } catch { self.error = error.localizedDescription }
                            preparingPreview = false
                        }
                    }
                    .accessibilityHint("タップして拡大")
            } else if let error {
                Label("画像を表示できません: \(error)", systemImage: "photo")
                    .font(.caption).foregroundColor(.secondary)
            } else {
                ProgressView("画像を読み込み中…").frame(height: 120)
            }
        }
        .alert(
            "画像を開けません",
            isPresented: Binding(get: { image != nil && error != nil }, set: {
                if !$0 {
                    error = nil
                }
            })
        ) {
            Button("OK") { error = nil }
        } message: { Text(error ?? "") }
        .fullScreenCover(isPresented: Binding(get: { previewURL != nil }, set: {
            if !$0 {
                dismissPreview()
            }
        })) {
            if let previewURL {
                ConversationPreview(url: previewURL, isImage: true) { dismissPreview() }
            }
        }
        .task(id: LoadID(host: media.host, source: source)) {
            image = nil; original = nil; error = nil
            do {
                let loaded = try await loadConversationImage(source, media: media)
                try Task.checkCancellation()
                image = loaded.0
                original = loaded.1
            } catch {
                if !Task.isCancelled {
                    self.error = error.localizedDescription
                }
            }
        }
    }

    private func dismissPreview() {
        if let previewURL {
            try? FileManager.default.removeItem(at: previewURL.deletingLastPathComponent())
        }
        previewURL = nil
    }
}

@MainActor private func conversationImageData(_ image: SessionImage,
                                              media: ConversationMediaAccess) async throws -> Data {
    let source = image.source
    let data: Data
    if image.encoded {
        data = try await Task.detached(priority: .userInitiated) {
            guard let decoded = Data(base64Encoded: source) else { throw CocoaError(.fileReadCorruptFile) }
            return decoded
        }.value
    } else if source.hasPrefix("data:image/") {
        throw CocoaError(.fileReadCorruptFile)
    } else if let url = URL(string: source), url.scheme == "https" || url.scheme == "http" {
        let (downloaded, response) = try await URLSession.shared.data(from: url)
        guard let response = response as? HTTPURLResponse, (200 ..< 300).contains(response.statusCode) else {
            throw URLError(.badServerResponse)
        }
        data = downloaded
    } else {
        let path = try conversationFileURL(source, cwd: media.cwd).path
        let url = try await media.download(path)
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        data = try await Task.detached(priority: .userInitiated) { try Data(contentsOf: url) }.value
    }
    return data
}

@MainActor private func loadConversationImage(_ source: SessionImage,
                                              media: ConversationMediaAccess) async throws -> (UIImage, Data) {
    let data = try await conversationImageData(source, media: media)
    return try await Task.detached(priority: .userInitiated) {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let thumbnail = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                  kCGImageSourceCreateThumbnailFromImageAlways: true,
                  kCGImageSourceCreateThumbnailWithTransform: true,
                  kCGImageSourceThumbnailMaxPixelSize: 1600
              ] as CFDictionary) else { throw CocoaError(.fileReadCorruptFile) }
        return (UIImage(cgImage: thumbnail), data)
    }.value
}

private func writeConversationImage(_ data: Data) async throws -> URL {
    try Task.checkCancellation()
    let local = try await Task.detached(priority: .userInitiated) {
        try inTemporaryDirectory { directory in
            let image = CGImageSourceCreateWithData(data as CFData, nil)
            let type = image.flatMap { CGImageSourceGetType($0) }.flatMap { UTType($0 as String) }
            let local = directory.appendingPathComponent("image")
                .appendingPathExtension(type?.preferredFilenameExtension ?? "png")
            try data.write(to: local)
            return local
        }
    }.value
    if Task.isCancelled {
        try? FileManager.default.removeItem(at: local.deletingLastPathComponent())
        throw CancellationError()
    }
    return local
}

struct ConversationPreview: View {
    let url: URL
    let isImage: Bool
    let close: () -> Void
    @State private var saving = false
    @State private var saved = false
    @State private var saveError: String?

    var body: some View {
        NavigationStack {
            Group {
                if isImage {
                    ConversationImagePreview(url: url)
                } else {
                    ConversationFilePreview(url: url)
                }
            }
            .disabled(saving)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    HStack(spacing: 20) {
                        if isImage {
                            Button {
                                saving = true
                                Task { await save() }
                            } label: {
                                Image(systemName: saved ? "checkmark" : "arrow.down.to.line")
                            }
                            .disabled(saving || saved)
                            .accessibilityLabel(saved ? "保存済み" : "保存")
                            .accessibilityIdentifier("conversation.preview.save")
                        }
                        Button(action: close) { Image(systemName: "xmark") }
                            .disabled(saving)
                            .accessibilityLabel("閉じる")
                            .accessibilityIdentifier("conversation.preview.close")
                    }
                }
            }
            .alert("画像を保存できません", isPresented: Binding(get: { saveError != nil }, set: {
                if !$0 {
                    saveError = nil
                }
            })) {
                Button("OK") { saveError = nil }
            } message: { Text(saveError ?? "") }
        }
        .interactiveDismissDisabled(isImage || saving)
    }

    @MainActor private func save() async {
        defer { saving = false }
        let status = await PHPhotoLibrary.requestAuthorization(for: .addOnly)
        guard status == .authorized || status == .limited else {
            saveError = "設定でBexの写真への追加を許可してください。"
            return
        }
        do {
            try await PHPhotoLibrary.shared().performChanges {
                PHAssetCreationRequest.forAsset().addResource(with: .photo, fileURL: url, options: nil)
            }
            saved = true
        } catch { saveError = error.localizedDescription }
    }
}

private struct ConversationImagePreview: View {
    let url: URL
    @State private var image: UIImage?
    @State private var loadFailed = false

    var body: some View {
        Group {
            if let image {
                ZoomableConversationImage(image: image)
            } else if loadFailed {
                Text("画像ファイルを読み込めません。")
            } else {
                ProgressView("画像を読み込み中…")
            }
        }
        .task(id: url) {
            let loaded = await Task.detached(priority: .userInitiated) {
                UIImage(contentsOfFile: url.path)
            }.value
            guard !Task.isCancelled else { return }
            if let loaded {
                image = loaded
            } else {
                loadFailed = true
            }
        }
    }
}

private struct ZoomableConversationImage: UIViewRepresentable {
    let image: UIImage

    func makeUIView(context _: Context) -> ImageScrollView {
        ImageScrollView(image: image)
    }

    func updateUIView(_: ImageScrollView, context _: Context) {}

    final class ImageScrollView: UIScrollView, UIScrollViewDelegate {
        private let imageView: UIImageView
        private var viewport = CGSize.zero

        init(image: UIImage) {
            imageView = UIImageView(image: image)
            super.init(frame: .zero)
            imageView.contentMode = .scaleAspectFit
            imageView.isAccessibilityElement = true
            imageView.accessibilityLabel = "画像プレビュー"
            imageView.accessibilityIdentifier = "conversation.preview.image"
            addSubview(imageView)
            delegate = self
            maximumZoomScale = 6
            showsHorizontalScrollIndicator = false
            showsVerticalScrollIndicator = false
        }

        @available(*, unavailable)
        required init?(coder _: NSCoder) {
            fatalError("init(coder:) has not been implemented")
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            guard bounds.size != viewport else { return }
            viewport = bounds.size
            setZoomScale(1, animated: false)
            imageView.frame = CGRect(origin: .zero, size: viewport)
            contentSize = viewport
        }

        func viewForZooming(in _: UIScrollView) -> UIView? {
            imageView
        }
    }
}

private struct ConversationFilePreview: UIViewControllerRepresentable {
    let url: URL

    func makeCoordinator() -> Coordinator {
        Coordinator(url: url)
    }

    func makeUIViewController(context: Context) -> QLPreviewController {
        let controller = QLPreviewController()
        controller.dataSource = context.coordinator
        controller.isModalInPresentation = true
        return controller
    }

    func updateUIViewController(_: QLPreviewController, context _: Context) {}

    final class Coordinator: NSObject, QLPreviewControllerDataSource {
        let url: URL
        init(url: URL) {
            self.url = url
        }

        func numberOfPreviewItems(in _: QLPreviewController) -> Int {
            1
        }

        func previewController(_: QLPreviewController, previewItemAt _: Int) -> QLPreviewItem {
            url as NSURL
        }
    }
}
