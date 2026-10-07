import AgentCore
import PhotosUI
import QuickLook
import SwiftUI
import UniformTypeIdentifiers

/// "+": Photo Library or Choose Files, admitted by core and attached to `draftKey`.
struct AttachmentButton: View {
    @ObservedObject var model: BexAppViewModel
    let draftKey: String
    @State private var files = false
    @State private var photos = false
    @State private var selection: [PhotosPickerItem] = []
    @State private var host: String?

    var body: some View {
        Menu {
            Button("Photo Library", systemImage: "photo") { begin(); photos = true }
            Button("Choose Files", systemImage: "folder") { begin(); files = true }
        } label: {
            Image(systemName: "plus").font(.system(size: 20)).frame(width: 44, height: 44)
                .foregroundStyle(AppTheme.text)
        }
        .accessibilityLabel("Add attachment")
        .photosPicker(isPresented: $photos, selection: $selection, maxSelectionCount: 100, matching: .images)
        .onChange(of: selection) { _, items in
            guard !items.isEmpty else { return }
            let picked = items
            selection = []
            Task { await attachPhotos(picked) }
        }
        .fileImporter(isPresented: $files, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            Task {
                do { try await attachFiles(result.get()) } catch { model.notice = error.localizedDescription }
            }
        }
    }

    private func begin() {
        host = model.selectedProfileId
    }

    private func attachPhotos(_ items: [PhotosPickerItem]) async {
        var picked: [PickedFile] = []
        for item in items {
            do {
                guard let data = try await item.loadTransferable(type: Data.self) else { continue }
                let type = item.supportedContentTypes.first
                let ext = type?.preferredFilenameExtension ?? "jpg"
                let url = try AttachmentFiles.directory().appendingPathComponent("\(UUID().uuidString).\(ext)")
                try data.write(to: url, options: .atomic)
                picked.append(PickedFile(url: url, name: "Photo.\(ext)",
                                         mimeType: type?.preferredMIMEType ?? "image/jpeg"))
            } catch { model.notice = error.localizedDescription }
        }
        await attach(picked)
    }

    private func attachFiles(_ sources: [URL]) async throws {
        var picked: [PickedFile] = []
        for source in sources {
            let access = source.startAccessingSecurityScopedResource()
            defer {
                if access {
                    source.stopAccessingSecurityScopedResource()
                }
            }
            let type = try source.resourceValues(forKeys: [.contentTypeKey]).contentType
            let target = try AttachmentFiles.directory()
                .appendingPathComponent("\(UUID().uuidString)-\(source.lastPathComponent)")
            try FileManager.default.copyItem(at: source, to: target)
            picked.append(PickedFile(url: target, name: source.lastPathComponent,
                                     mimeType: type?.preferredMIMEType ?? ""))
        }
        await attach(picked)
    }

    private func attach(_ picked: [PickedFile]) async {
        guard !picked.isEmpty else { return }
        let existing = model.snapshot.draftAttachments(draftKey: draftKey)
        let admission = admitAttachments(existing: existing, candidates: picked.map {
            AttachmentCandidate(name: $0.name, mimeType: $0.mimeType, sizeBytes: $0.size)
        })
        var files: [LocalFile] = []
        for admitted in admission.accepted {
            let file = picked[Int(admitted.index)]
            if admitted.kind == .image,
               ImageSizing.needsRendering(mimeType: admitted.mimeType, needsCompression: admitted.needsCompression) {
                guard let rendered = await AttachmentFiles.renderJPEG(file.url) else {
                    model.notice = "Failed to read '\(file.name)'."
                    continue
                }
                files.append(LocalFile(path: rendered.path, name: ImageSizing.jpegName(admitted.name),
                                       mimeType: "image/jpeg"))
            } else {
                files.append(LocalFile(path: file.url.path, name: admitted.name, mimeType: admitted.mimeType))
            }
        }
        if let error = admission.error {
            model.notice = error
        }
        guard model.selectedProfileId == host, !files.isEmpty else { return }
        model.perform(.attachFiles(draftKey: draftKey, files: files))
    }
}

private struct PickedFile {
    let url: URL
    let name: String
    let mimeType: String
    var size: UInt64 {
        UInt64((try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0)
    }
}

enum AttachmentFiles {
    static func directory() throws -> URL {
        let directory = try FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true
        ).appendingPathComponent("DraftAttachments", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
    }

    /// Redraws an image (HEIC included) as a JPEG within the size limit.
    static func renderJPEG(_ source: URL) async -> URL? {
        await Task.detached(priority: .userInitiated) {
            guard let image = UIImage(contentsOfFile: source.path) else { return nil }
            let size = ImageSizing.scaled(CGSize(width: image.size.width * image.scale,
                                                 height: image.size.height * image.scale))
            let format = UIGraphicsImageRendererFormat.default()
            format.scale = 1
            let rendered = UIGraphicsImageRenderer(size: size, format: format).image { _ in
                image.draw(in: CGRect(origin: .zero, size: size))
            }
            guard let data = rendered.jpegData(compressionQuality: ImageSizing.jpegQuality),
                  let target = try? directory().appendingPathComponent("\(UUID().uuidString).jpg"),
                  (try? data.write(to: target, options: .atomic)) != nil else { return nil }
            try? FileManager.default.removeItem(at: source)
            return target
        }.value
    }
}

/// A draft's media: 72pt tiles with remove and upload state.
struct ComposerAttachmentStrip: View {
    let attachments: [DraftAttachment]
    let draftKey: String?
    @ObservedObject var model: BexAppViewModel

    var body: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 8.75) {
                ForEach(attachments, id: \.id) { attachment in
                    DraftAttachmentTile(attachment: attachment, size: 72, radius: 16) {
                        model.perform(.removeAttachment(draftKey: draftKey, id: attachment.id))
                    } retry: {
                        model.perform(.retryAttachment(draftKey: draftKey, id: attachment.id))
                    }
                }
            }
        }
        .scrollIndicators(.hidden)
    }
}

struct DraftAttachmentTile: View {
    let attachment: DraftAttachment
    let size: CGFloat
    let radius: CGFloat
    var remove: (() -> Void)?
    var retry: (() -> Void)?

    var body: some View {
        ZStack(alignment: .topTrailing) {
            thumbnail
                .frame(width: size, height: size)
                .background(AppTheme.subtleStrong)
                .clipShape(RoundedRectangle(cornerRadius: radius))
            if let remove, size > 40 {
                Button(action: remove) {
                    Image(systemName: "xmark").font(.system(size: 9, weight: .bold)).foregroundStyle(.white)
                        .frame(width: 22, height: 22).background(.black.opacity(0.55), in: Circle())
                }
                .buttonStyle(.plain)
                .padding(4)
                .accessibilityLabel("Remove \(attachment.name)")
            }
        }
        .overlay(alignment: .bottomLeading) {
            if attachment.status == "uploading" || attachment.status == "failed" {
                Button { retry?() } label: {
                    HStack(spacing: 2) {
                        Image(systemName: attachment.status == "failed" ? "arrow.clockwise" : "arrow.up")
                            .font(.system(size: 8))
                        if size > 40 {
                            Text(attachment.status == "failed" ? "Retry" : "Uploading").font(AppTheme.font(10))
                        }
                    }
                    .foregroundStyle(.white)
                    .padding(.horizontal, 4).padding(.vertical, 2)
                    .background(.black.opacity(0.7), in: Capsule())
                }
                .buttonStyle(.plain)
                .disabled(attachment.status != "failed")
                .padding(1.75)
                .accessibilityHint(attachment.error ?? "")
            }
        }
    }

    @ViewBuilder
    private var thumbnail: some View {
        if attachment.kind == "image", let image = UIImage(contentsOfFile: attachment.localPath) {
            Image(uiImage: image).resizable().scaledToFill()
        } else {
            Image(systemName: attachment.kind == "image" ? "photo" : "doc").foregroundStyle(AppTheme.muted)
        }
    }
}

/// A sent message's attachments: images full width, files as cards.
struct MessageAttachments: View {
    let attachments: [Attachment]
    let download: (String, String) async throws -> URL

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            ForEach(attachments, id: \.id) { attachment in
                MessageAttachmentView(attachment: attachment, download: download)
            }
        }
    }
}

private struct MessageAttachmentView: View {
    let attachment: Attachment
    let download: (String, String) async throws -> URL
    @State private var local: URL?
    @State private var preview: URL?
    @State private var failed = false

    var body: some View {
        Button { Task { await open() } } label: {
            if attachment.kind == .image {
                Group {
                    if let local, let image = UIImage(contentsOfFile: local.path) {
                        Image(uiImage: image).resizable().scaledToFill()
                    } else {
                        Image(systemName: failed ? "exclamationmark.triangle" : "photo")
                            .foregroundStyle(AppTheme.muted)
                    }
                }
                .frame(maxWidth: .infinity).aspectRatio(1.3, contentMode: .fit)
                .background(AppTheme.text.opacity(0.15))
                .clipShape(RoundedRectangle(cornerRadius: 14))
            } else {
                HStack(spacing: 10) {
                    Image(systemName: "doc").frame(width: 35, height: 42)
                        .background(AppTheme.subtle, in: RoundedRectangle(cornerRadius: 6))
                    VStack(alignment: .leading, spacing: 2) {
                        Text(attachment.name).font(AppTheme.font(14, weight: .medium)).lineLimit(2)
                        Text(ByteCountFormatter.string(fromByteCount: Int64(attachment.size), countStyle: .file))
                            .font(AppTheme.font(13)).foregroundStyle(AppTheme.muted)
                    }
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.right").font(.system(size: 12)).foregroundStyle(AppTheme.muted)
                }
                .foregroundStyle(AppTheme.text)
                .padding(10.5)
                .frame(width: 280)
                .background(AppTheme.card, in: RoundedRectangle(cornerRadius: 10.5))
                .overlay(RoundedRectangle(cornerRadius: 10.5).stroke(AppTheme.border))
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Open \(attachment.name)")
        .quickLookPreview($preview)
        .task(id: attachment.id) {
            if attachment.kind == .image, local == nil {
                await load()
            }
        }
    }

    private func load() async {
        do {
            local = try await download(attachment.id, attachment.name)
            failed = false
        } catch { failed = true }
    }

    private func open() async {
        if local == nil {
            await load()
        }
        preview = local
    }
}
