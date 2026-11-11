import AgentCore
import ImageIO
import Photos
import QuickLook
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Host paths always use the authenticated transfer; never read a Host path from the phone's filesystem.
func conversationFilePath(_ source: String, cwd: String) throws -> String {
    guard let target = markdownFileTarget(source: source, cwd: cwd, surface: .mobile) else {
        throw URLError(.unsupportedURL)
    }
    return target.path
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
    struct ResolvedFile {
        let path: String
        let isDirectory: Bool
        let line: UInt32?
    }

    let host: String?
    let cwd: String
    let home: String?
    let draft: DraftKey
    let presentation: ConversationMediaPresentation
    let download: @MainActor (String) async throws -> URL
    let resolve: @MainActor (String) async throws -> ResolvedFile
    let openDirectory: @MainActor (String) -> Void
    let openFile: @MainActor (String, UInt32?) -> Void
}

struct ConversationImageSkeleton: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    var label = "画像を読み込み中"
    var identifier = "image.loading.skeleton"

    var body: some View {
        RoundedRectangle(cornerRadius: 10)
            .fill(Color(UIColor.secondarySystemBackground))
            .phaseAnimator(reduceMotion ? [1.0] : [1.0, 0.5]) { $0.opacity($1) } animation: { _ in
                .easeInOut(duration: 0.9)
            }
            .frame(maxWidth: 320)
            .frame(height: 320)
            .accessibilityLabel(label)
            .accessibilityIdentifier(identifier)
    }
}

struct ConversationImage: View {
    @Environment(\.locale) private var locale
    let source: SessionImage
    let label: String
    let identifier: String
    let media: ConversationMediaAccess
    var contentMode: ContentMode = .fit
    @State private var image: UIImage?
    @State private var original: Data?
    @State private var error: String?
    private struct LoadID: Equatable {
        let host: String?
        let cwd: String
        let source: SessionImage
    }

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image).resizable().aspectRatio(contentMode: contentMode)
                    .frame(maxWidth: .infinity, maxHeight: 420)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                    .accessibilityLabel(label.isEmpty ? Text("画像") : Text(verbatim: label))
                    .accessibilityIdentifier(identifier)
                    .onTapGesture {
                        if let original {
                            media.presentation.openImage(original)
                        }
                    }
                    .accessibilityHint("タップして拡大")
            } else if let error {
                Label(
                    localizedUiFormat(language: locale.appLanguage, source: "画像を表示できません: {0}",
                                      arguments: [localizedErrorMessage(
                                          language: locale.appLanguage,
                                          error: error
                                      )]),
                    systemImage: "photo"
                )
                .font(.caption).foregroundColor(.secondary)
            } else {
                ConversationImageSkeleton()
            }
        }
        .task(id: LoadID(host: media.host, cwd: media.cwd, source: source)) {
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
        let path = try conversationFilePath(source, cwd: media.cwd)
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

func writeConversationImage(_ data: Data) async throws -> URL {
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
    let file: SharedFile
    @Environment(\.dismiss) private var dismiss
    @State private var saving = false
    @State private var saved = false
    @State private var saveError: String?

    var body: some View {
        NavigationStack {
            ConversationFilePreview(url: file.url)
                .accessibilityIdentifier("conversation.preview.content")
                .navigationTitle(file.url.lastPathComponent)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("閉じる", systemImage: "xmark") { dismiss() }
                            .labelStyle(.iconOnly)
                            .disabled(saving)
                            .accessibilityIdentifier("conversation.preview.close")
                    }
                    ToolbarItemGroup(placement: .primaryAction) {
                        Button {
                            saving = true
                            Task { await save() }
                        } label: {
                            Image(systemName: saved ? "checkmark" : "arrow.down.to.line")
                        }
                        .disabled(saving || saved)
                        .accessibilityLabel(LocalizedStringKey(saved ? "保存済み" : "保存"))
                        .accessibilityIdentifier("conversation.preview.save")
                        ShareLink(item: file.url) { Image(systemName: "square.and.arrow.up") }
                            .accessibilityLabel("保存・共有")
                            .accessibilityIdentifier("conversation.preview.share")
                    }
                }
                .alert("保存できません", isPresented: Binding(get: { saveError != nil }, set: {
                    if !$0 {
                        saveError = nil
                    }
                })) {
                    Button("OK") { saveError = nil }
                } message: { ErrorMessage(source: saveError ?? "") }
        }
        .interactiveDismissDisabled(saving)
    }

    @MainActor private func save() async {
        defer { saving = false }
        let status = await PHPhotoLibrary.requestAuthorization(for: .addOnly)
        guard status == .authorized || status == .limited else {
            saveError = "設定でBexの写真への追加を許可してください。"
            return
        }
        do {
            let url = file.url
            let resourceType: PHAssetResourceType = UTType(filenameExtension: url.pathExtension)?
                .conforms(to: .image) == true ? .photo : .video
            try await PHPhotoLibrary.shared().performChanges {
                PHAssetCreationRequest.forAsset().addResource(with: resourceType, fileURL: url, options: nil)
            }
            saved = true
        } catch { saveError = error.localizedDescription }
    }
}

struct ConversationFilePreview: UIViewControllerRepresentable {
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

    func updateUIViewController(_ controller: QLPreviewController, context: Context) {
        guard context.coordinator.url != url else { return }
        context.coordinator.url = url
        controller.reloadData()
    }

    final class Coordinator: NSObject, QLPreviewControllerDataSource {
        var url: URL
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
