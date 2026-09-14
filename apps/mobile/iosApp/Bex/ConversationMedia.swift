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

/// Only the authenticated file and gallery operations are available to media views.
struct ConversationMediaAccess {
    let host: String?
    let cwd: String
    let download: @MainActor (String) async throws -> URL
    let sessionImages: (@MainActor () async throws -> [SessionImage])?
}

struct ConversationImage: View {
    let source: SessionImage
    let label: String
    let identifier: String
    let media: ConversationMediaAccess
    var onSelect: (() -> Void)?
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
                Image(uiImage: image).resizable().scaledToFit()
                    .frame(maxWidth: .infinity, maxHeight: 420)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                    .accessibilityLabel(label.isEmpty ? "画像" : label)
                    .accessibilityIdentifier(identifier)
                    .onTapGesture {
                        if let onSelect {
                            onSelect(); return
                        }
                        guard !preparingPreview, let original else { return }
                        preparingPreview = true
                        Task {
                            do {
                                previewURL = try await Task.detached(priority: .userInitiated) {
                                    try writeConversationImage(original)
                                }.value
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
                ConversationPreview(url: previewURL, isImage: true, media: media, source: source) { dismissPreview() }
            }
        }
        .task(id: LoadID(host: media.host, source: source)) {
            image = nil; original = nil; error = nil
            do {
                let loaded = try await loadConversationImage(
                    source,
                    media: media,
                    maxPixelSize: onSelect == nil ? 1600 : 160
                )
                try Task.checkCancellation()
                image = loaded.0
                original = onSelect == nil ? loaded.1 : nil
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
        guard let decoded = Data(base64Encoded: source) else { throw CocoaError(.fileReadCorruptFile) }
        data = decoded
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

@MainActor private func loadConversationImage(_ source: SessionImage, media: ConversationMediaAccess,
                                              maxPixelSize: Int = 1600) async throws -> (UIImage, Data) {
    let data = try await conversationImageData(source, media: media)
    return try await Task.detached(priority: .userInitiated) {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let thumbnail = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                  kCGImageSourceCreateThumbnailFromImageAlways: true,
                  kCGImageSourceCreateThumbnailWithTransform: true,
                  kCGImageSourceThumbnailMaxPixelSize: maxPixelSize
              ] as CFDictionary) else { throw CocoaError(.fileReadCorruptFile) }
        return (UIImage(cgImage: thumbnail), data)
    }.value
}

private func writeConversationImage(_ data: Data) throws -> URL {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    do {
        let image = CGImageSourceCreateWithData(data as CFData, nil)
        let type = image.flatMap { CGImageSourceGetType($0) }.flatMap { UTType($0 as String) }
        let local = directory.appendingPathComponent("image")
            .appendingPathExtension(type?.preferredFilenameExtension ?? "png")
        try data.write(to: local)
        return local
    } catch {
        try? FileManager.default.removeItem(at: directory)
        throw error
    }
}

struct ConversationPreview: View {
    let url: URL
    let isImage: Bool
    var media: ConversationMediaAccess?
    var source: SessionImage?
    let close: () -> Void
    @State private var sources: [SessionImage] = []
    @State private var selected: SessionImage?
    @State private var downloaded: [SessionImage: URL] = [:]
    @State private var galleryError: String?
    private var displayedURL: URL? {
        guard let selected, selected != source else { return url }
        return downloaded[selected]
    }

    @State private var saving = false
    @State private var saved = false
    @State private var saveError: String?

    var body: some View {
        NavigationStack {
            VStack {
                if let galleryError {
                    Text(galleryError).font(.caption).foregroundColor(.red)
                }
                if let displayedURL {
                    ConversationFilePreview(url: displayedURL) { step in
                        guard isImage, !saving,
                              let index = sources.firstIndex(where: { $0 == (selected ?? source) }),
                              sources.indices.contains(index + step) else { return }
                        selected = sources[index + step]
                    }
                } else {
                    ProgressView("画像を読み込み中…")
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    if let index = sources.firstIndex(where: { $0 == (selected ?? source) }) {
                        Text("\(index + 1) / \(sources.count)")
                            .accessibilityIdentifier("conversation.preview.position")
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    HStack(spacing: 20) {
                        if isImage {
                            Button {
                                saving = true
                                Task { await save() }
                            } label: {
                                Image(systemName: saved ? "checkmark" : "arrow.down.to.line")
                            }
                            .disabled(saving || saved || displayedURL == nil)
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
        .interactiveDismissDisabled(saving)
        .task {
            guard isImage, let load = media?.sessionImages else { return }
            do {
                let images = try await load()
                guard !Task.isCancelled else { return }
                sources = images
            } catch {
                if !Task.isCancelled {
                    galleryError = error.localizedDescription
                }
            }
        }
        .task(id: selected) {
            saved = false
            guard let selected, selected != source, let media else { return }
            guard downloaded[selected] == nil else { return }
            do {
                let data = try await conversationImageData(selected, media: media)
                try Task.checkCancellation()
                let local = try await Task.detached(priority: .userInitiated) {
                    try writeConversationImage(data)
                }.value
                if Task
                    .isCancelled {
                    try? FileManager.default.removeItem(at: local.deletingLastPathComponent()); return
                }
                downloaded[selected] = local
                galleryError = nil
            } catch {
                if !Task.isCancelled {
                    galleryError = error.localizedDescription
                }
            }
        }
        .onDisappear {
            for local in downloaded
                .values {
                try? FileManager.default.removeItem(at: local.deletingLastPathComponent())
            }
            downloaded.removeAll()
        }
    }

    @MainActor private func save() async {
        defer { saving = false }
        guard let displayedURL else { return }
        let status = await PHPhotoLibrary.requestAuthorization(for: .addOnly)
        guard status == .authorized || status == .limited else {
            saveError = "設定でBexの写真への追加を許可してください。"
            return
        }
        do {
            try await PHPhotoLibrary.shared().performChanges {
                PHAssetCreationRequest.forAsset().addResource(with: .photo, fileURL: displayedURL, options: nil)
            }
            saved = true
        } catch { saveError = error.localizedDescription }
    }
}

private struct ConversationFilePreview: UIViewControllerRepresentable {
    let url: URL
    let swipe: (Int) -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(url: url, swipe: swipe)
    }

    func makeUIViewController(context: Context) -> QLPreviewController {
        let controller = QLPreviewController()
        controller.dataSource = context.coordinator
        for direction: UISwipeGestureRecognizer.Direction in [.left, .right] {
            let gesture = UISwipeGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.swiped))
            gesture.direction = direction
            gesture.delegate = context.coordinator
            controller.view.addGestureRecognizer(gesture)
        }
        return controller
    }

    func updateUIViewController(_ controller: QLPreviewController, context: Context) {
        context.coordinator.swipe = swipe
        if context.coordinator.url != url {
            context.coordinator.url = url
            controller.reloadData()
        }
    }

    final class Coordinator: NSObject, QLPreviewControllerDataSource, UIGestureRecognizerDelegate {
        var url: URL
        var swipe: (Int) -> Void
        init(url: URL, swipe: @escaping (Int) -> Void) {
            self.url = url
            self.swipe = swipe
        }

        @objc func swiped(_ gesture: UISwipeGestureRecognizer) {
            swipe(gesture.direction == .left ? 1 : -1)
        }

        func gestureRecognizer(
            _: UIGestureRecognizer,
            shouldRecognizeSimultaneouslyWith _: UIGestureRecognizer
        ) -> Bool {
            true
        }

        func numberOfPreviewItems(in _: QLPreviewController) -> Int {
            1
        }

        func previewController(_: QLPreviewController, previewItemAt _: Int) -> QLPreviewItem {
            url as NSURL
        }
    }
}
