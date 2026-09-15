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
    var visualization: (@MainActor (String) async throws -> String)?
}

struct ConversationImage: View {
    let source: SessionImage
    let label: String
    let identifier: String
    let media: ConversationMediaAccess
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
                    maxPixelSize: 1600
                )
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
    @State private var selected = 0
    @State private var downloaded: [SessionImage: URL] = [:]
    @State private var galleryError: String?
    private var urls: [URL] {
        sources.isEmpty ? [url] : sources.map { $0 == source ? url : downloaded[$0] ?? url }
    }

    private var displayedURL: URL? {
        guard sources.indices.contains(selected), sources[selected] != source else { return url }
        return downloaded[sources[selected]]
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
                ConversationFilePreview(urls: urls, selected: $selected)
                    .disabled(saving)
                    .overlay {
                        if displayedURL == nil {
                            ProgressView("画像を読み込み中…")
                                .frame(maxWidth: .infinity, maxHeight: .infinity)
                                .background(Color(uiColor: .systemBackground))
                        }
                    }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    if !sources.isEmpty {
                        Text("\(selected + 1) / \(sources.count)")
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
        .interactiveDismissDisabled(isImage || saving)
        .task {
            guard isImage, let load = media?.sessionImages else { return }
            do {
                let images = try await load()
                guard !Task.isCancelled else { return }
                guard let source, let index = images.firstIndex(of: source) else { return }
                for image in images where image != source {
                    guard let media else { break }
                    let data = try await conversationImageData(image, media: media)
                    try Task.checkCancellation()
                    downloaded[image] = try writeConversationImage(data)
                }
                // Publish the gallery once its URLs are stable. Reloading Quick
                // Look as downloads finish can reset an in-progress swipe.
                selected = index
                sources = images
            } catch {
                if !Task.isCancelled {
                    galleryError = error.localizedDescription
                }
            }
        }
        .onChange(of: selected) { _ in saved = false }
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
    let urls: [URL]
    @Binding var selected: Int

    func makeCoordinator() -> Coordinator {
        Coordinator(urls: urls, selected: $selected)
    }

    func makeUIViewController(context: Context) -> QLPreviewController {
        let controller = QLPreviewController()
        controller.dataSource = context.coordinator
        controller.isModalInPresentation = true
        controller.currentPreviewItemIndex = selected
        let coordinator = context.coordinator
        coordinator.observation = controller.observe(
            \.currentPreviewItemIndex, options: [.new]
        ) { [weak coordinator] _, change in
            guard let coordinator, !coordinator.updating, let index = change.newValue,
                  coordinator.urls.indices.contains(index) else { return }
            coordinator.selected.wrappedValue = index
        }
        return controller
    }

    func updateUIViewController(_ controller: QLPreviewController, context: Context) {
        context.coordinator.updating = true
        defer { context.coordinator.updating = false }
        context.coordinator.selected = $selected
        if context.coordinator.urls != urls {
            context.coordinator.urls = urls
            controller.reloadData()
            // Quick Look owns selection while swiping. Only a new gallery
            // supplies a programmatic index; view refreshes must not restore it.
            if controller.currentPreviewItemIndex != selected {
                controller.currentPreviewItemIndex = selected
            }
        }
    }

    final class Coordinator: NSObject, QLPreviewControllerDataSource {
        var urls: [URL]
        var selected: Binding<Int>
        var observation: NSKeyValueObservation?
        var updating = false
        init(urls: [URL], selected: Binding<Int>) {
            self.urls = urls
            self.selected = selected
        }

        func numberOfPreviewItems(in _: QLPreviewController) -> Int {
            urls.count
        }

        func previewController(_: QLPreviewController, previewItemAt index: Int) -> QLPreviewItem {
            urls[index] as NSURL
        }
    }
}
