import AgentCore
import SwiftUI
import UIKit
import UniformTypeIdentifiers

struct ComposerAttachmentStrip: View {
    @Environment(\.locale) private var locale
    let attachments: [Attachment]
    let media: ConversationMediaAccess
    let remove: (Int) -> Void

    var body: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 12) {
                ForEach(Array(attachments.enumerated()), id: \.element.path) { index, attachment in
                    Group {
                        if attachment.isImage {
                            ConversationImage(source: SessionImage(reference: attachment.path),
                                              label: attachment.name,
                                              identifier: "composer.attachment.\(index)",
                                              media: media,
                                              contentMode: .fill)
                                .frame(width: 120, height: 120)
                        } else {
                            Label(attachment.name, systemImage: "doc")
                                .lineLimit(1).padding(12).padding(.trailing, 24)
                        }
                    }
                    .background(Color(UIColor.secondarySystemBackground))
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                    .overlay(alignment: .topTrailing) {
                        Button { remove(index) } label: {
                            Image(systemName: "xmark.circle.fill")
                                .font(.title3)
                                .symbolRenderingMode(.hierarchical)
                                .foregroundStyle(.secondary)
                                .frame(width: 44, height: 44, alignment: .topTrailing)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .offset(x: 4, y: -4)
                        .accessibilityLabel(localizedUiFormat(
                            language: locale.appLanguage,
                            source: "{0}を外す",
                            arguments: [attachment.name]
                        ))
                    }
                }
            }.padding(8)
        }
        .frame(height: attachments.contains(where: \.isImage) ? 136 : 60)
    }
}

extension ConversationModel {
    func removeAttachment(_ id: Int) {
        perform(.removeAttachment(draftKey: coreDraftKey, index: UInt32(id))) { [weak self] _ in self?.persist() }
    }

    func attach(_ url: URL, temporaryDirectory: URL? = nil) async throws {
        let access = url.startAccessingSecurityScopedResource()
        transferring = true
        transferError = nil
        defer {
            persist()
            if access {
                url.stopAccessingSecurityScopedResource()
            }
            if let temporaryDirectory {
                try? FileManager.default.removeItem(at: temporaryDirectory)
            }
            transferring = false
        }
        let attachment = Attachment(path: url.path, name: url.lastPathComponent,
                                    isImage: UTType(filenameExtension: url.pathExtension)?.conforms(to: .image) == true)
        _ = try await outcome(for: .uploadAttachment(UploadAttachment(
            draftKey: coreDraftKey,
            attachment: attachment,
            snapshot: nil
        )))
    }
}
