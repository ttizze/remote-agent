import AgentCore
import SwiftUI
import UniformTypeIdentifiers

struct DeviceRecordingDocument: FileDocument {
    static var readableContentTypes: [UTType] {
        [.data, .movie]
    }

    let data: Data

    init(data: Data) {
        self.data = data
    }

    init(configuration: ReadConfiguration) throws {
        data = configuration.file.regularFileContents ?? Data()
    }

    func fileWrapper(configuration _: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
    }
}

struct DeviceRecordingFileType {
    let mime: String
    let type: UTType
}

func deviceRecordingFileType(
    _ fileName: String,
    _ mimeType: String,
    _ bytes: [UInt8]
) -> DeviceRecordingFileType? {
    guard fileName.lowercased().hasSuffix(".mp4"),
          mimeType.lowercased() == "video/mp4",
          bytes.count >= 12,
          Array(bytes[4 ..< 8]) == Array("ftyp".utf8) else { return nil }
    return DeviceRecordingFileType(
        mime: mimeType,
        type: UTType(filenameExtension: "mp4") ?? .movie
    )
}

struct DeviceRecordingCard: View {
    @ObservedObject var model: BexAppViewModel
    let recording: DeviceRecordingView
    @Binding var document: DeviceRecordingDocument?
    @Binding var exporting: Bool
    @Binding var fileName: String
    @Binding var contentType: UTType

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Recording ready · \(recording.frameCount) frames · \(recording.byteCount) bytes")
                .font(AppTheme.font(12)).foregroundStyle(AppTheme.muted)
            HStack(spacing: 8) {
                Button("Save recording", action: save)
                Button("Attach recording", action: attach)
            }
        }
    }

    private func save() {
        guard let file = deviceRecordingFileType(recording.fileName, recording.mimeType, recording.bytes) else {
            model.notice = "The Host did not return a playable device recording."
            return
        }
        document = DeviceRecordingDocument(data: Data(recording.bytes))
        fileName = recording.fileName
        contentType = file.type
        exporting = true
    }

    private func attach() {
        do {
            let directory = try AttachmentFiles.directory()
            guard let file = deviceRecordingFileType(recording.fileName, recording.mimeType, recording.bytes) else {
                model.notice = "The Host did not return a playable device recording."
                return
            }
            let url = directory.appendingPathComponent(recording.fileName)
            try Data(recording.bytes).write(to: url, options: .atomic)
            model.perform(.attachFiles(
                draftKey: model.snapshot.currentDraftKey(),
                files: [LocalFile(path: url.path, name: url.lastPathComponent, mimeType: file.mime)]
            ))
        } catch {
            model.notice = error.localizedDescription
        }
    }
}
