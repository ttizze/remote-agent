import AgentCore
import PhotosUI
import QuickLook
import SwiftUI
import UniformTypeIdentifiers

struct ComposerAttachmentButton: View {
    @ObservedObject var model: BexAppViewModel
    @State private var files = false
    @State private var photos = false
    @State private var selection: [PhotosPickerItem] = []
    @State private var host: String?
    @State private var draftKey = ""
    var body: some View {
        Menu {
            Button("Photo Library", systemImage: "photo") { begin(); photos = true }
            Button("Choose Files", systemImage: "folder") { begin(); files = true }
        } label: { Image(systemName: "plus").frame(width: 32, height: 32) }
            .accessibilityLabel("Add attachment").disabled(!model.conversation.composer.canEdit)
            .photosPicker(isPresented: $photos, selection: $selection, maxSelectionCount: 100, matching: .images)
            .onChange(of: selection) { _, items in
                let selectedHost = host
                let key = draftKey
                Task {
                    for item in items {
                        do {
                            guard let data = try await item.loadTransferable(type: Data.self),
                                  let image = UIImage(data: data) else { continue }
                            let scale = min(1, 2048 / max(image.size.width, image.size.height))
                            let size = CGSize(width: image.size.width * scale, height: image.size.height * scale)
                            let resized = UIGraphicsImageRenderer(size: size).image { _ in image.draw(in: CGRect(
                                origin: .zero,
                                size: size
                            )) }
                            guard let bytes = resized.jpegData(compressionQuality: 0.85) else { continue }
                            let target = try attachmentDirectory().appendingPathComponent("\(UUID().uuidString).jpg")
                            try bytes.write(to: target, options: .atomic)
                            guard model.selectedProfileId == selectedHost
                            else { try? FileManager.default.removeItem(at: target); return }
                            model.perform(.attachFile(
                                path: target.path,
                                name: "Photo.jpg",
                                mimeType: "image/jpeg",
                                draftKey: key
                            ))
                        } catch { model.notice = error.localizedDescription }
                    }
                    selection = []
                }
            }
            .fileImporter(isPresented: $files, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
                do {
                    for source in try result.get() {
                        let access = source.startAccessingSecurityScopedResource()
                        defer {
                            if access {
                                source.stopAccessingSecurityScopedResource()
                            }
                        }
                        let values = try source.resourceValues(forKeys: [
                            .fileSizeKey,
                            .isRegularFileKey,
                            .contentTypeKey
                        ])
                        guard values.isRegularFile == true,
                              (values.fileSize ?? 0) <= 50 * 1024 * 1024 else { throw CocoaError(.fileReadTooLarge) }
                        let target = try attachmentDirectory()
                            .appendingPathComponent("\(UUID().uuidString)-\(source.lastPathComponent)")
                        try FileManager.default.copyItem(at: source, to: target)
                        guard model.selectedProfileId == host
                        else { try? FileManager.default.removeItem(at: target); return }
                        model.perform(.attachFile(
                            path: target.path,
                            name: source.lastPathComponent,
                            mimeType: values.contentType?.preferredMIMEType ?? "application/octet-stream",
                            draftKey: draftKey
                        ))
                    }
                } catch { model.notice = error.localizedDescription }
            }
    }

    private func begin() {
        host = model.selectedProfileId; draftKey = model.snapshot.currentDraftKey()
    }

    private func attachmentDirectory() throws -> URL {
        let directory = try FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        ).appendingPathComponent("DraftAttachments", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
    }
}

struct ConversationAttachmentStrip: View {
    let attachments: [DraftAttachment]
    let download: (String, String) async throws -> URL
    var perform: ((Intent) -> Void)?
    var body: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 8) {
                ForEach(attachments, id: \.id) { attachment in
                    ConversationAttachmentTile(attachment: attachment, download: download, perform: perform)
                }
            }
        }.scrollIndicators(.hidden)
    }
}

private struct ConversationAttachmentTile: View {
    let attachment: DraftAttachment
    let download: (String, String) async throws -> URL
    let perform: ((Intent) -> Void)?
    @State private var downloaded: URL?
    @State private var preview: URL?
    @State private var error: String?
    var body: some View {
        VStack(spacing: 4) {
            Button { Task { await open() } } label: {
                if let path = localURL?.path, attachment.kind == "image", let image = UIImage(contentsOfFile: path) {
                    Image(uiImage: image).resizable().scaledToFill().frame(width: 72, height: 72).clipped()
                } else {
                    Image(systemName: attachment.kind == "image" ? "photo" : "doc").frame(width: 72, height: 72)
                }
            }.clipShape(RoundedRectangle(cornerRadius: 16)).accessibilityLabel("Open \(attachment.name)")
            Text(attachment.name).font(T3Theme.font(11)).lineLimit(1).frame(width: 88)
            if attachment.status == "uploading" {
                ProgressView().controlSize(.mini)
            }
            if attachment.status == "failed", let perform {
                Button("Retry") { perform(.retryAttachment(id: attachment.id)) }.font(T3Theme.font(11))
                    .accessibilityHint(attachment.error ?? "Upload failed")
            }
            if let perform {
                Button("Remove", systemImage: "xmark") { perform(.removeAttachment(id: attachment.id)) }
                    .labelStyle(.iconOnly)
            }
            if let error {
                Text(error).font(T3Theme.font(11)).foregroundStyle(T3Theme.color("errorForeground"))
            }
        }.quickLookPreview($preview)
            .task(id: attachment.remoteId) {
                if attachment.kind == "image", localURL == nil {
                    await load()
                }
            }
    }

    private var localURL: URL? {
        attachment.localPath.isEmpty ? downloaded : URL(fileURLWithPath: attachment.localPath)
    }

    private func load() async {
        guard let id = attachment.remoteId else { return }
        do { downloaded = try await download(id, attachment.name); error = nil } catch {
            self.error = error.localizedDescription
        }
    }

    private func open() async {
        if localURL == nil {
            await load()
        }; preview = localURL
    }
}
