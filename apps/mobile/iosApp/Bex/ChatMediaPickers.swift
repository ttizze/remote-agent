import CoreTransferable
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Picker-owned URLs expire after their callback. Retain only the selected file
/// in a private temporary directory until the existing upload completes.
func retainChatMedia(_ source: URL) throws -> URL {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    do {
        let destination = directory.appendingPathComponent(source.lastPathComponent)
        try FileManager.default.copyItem(at: source, to: destination)
        return destination
    } catch {
        try? FileManager.default.removeItem(at: directory)
        throw error
    }
}

struct ChatMedia: Transferable {
    let url: URL

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .movie) {
            try Self(url: retainChatMedia($0.file))
        }
        FileRepresentation(importedContentType: .image) {
            try Self(url: retainChatMedia($0.file))
        }
    }
}

struct ChatCameraPicker: UIViewControllerRepresentable {
    let completion: (Result<URL?, Error>) -> Void
    func makeCoordinator() -> Coordinator {
        Coordinator(completion: completion)
    }

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.mediaTypes = (UIImagePickerController.availableMediaTypes(for: .camera) ?? []).filter {
            $0 == UTType.image.identifier || $0 == UTType.movie.identifier
        }
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_: UIImagePickerController, context _: Context) {}

    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let completion: (Result<URL?, Error>) -> Void
        init(completion: @escaping (Result<URL?, Error>) -> Void) {
            self.completion = completion
        }

        func imagePickerControllerDidCancel(_: UIImagePickerController) {
            completion(.success(nil))
        }

        func imagePickerController(
            _: UIImagePickerController,
            didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]
        ) {
            completion(Result {
                if let movie = info[.mediaURL] as? URL {
                    return try retainChatMedia(movie)
                }
                guard let image = info[.originalImage] as? UIImage,
                      let data = image.jpegData(compressionQuality: 0.9)
                else {
                    throw CocoaError(.fileReadCorruptFile)
                }
                let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
                    UUID().uuidString,
                    isDirectory: true
                )
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
                let destination = directory.appendingPathComponent("photo.jpg")
                do {
                    try data.write(to: destination)
                    return destination
                } catch {
                    try? FileManager.default.removeItem(at: directory)
                    throw error
                }
            })
        }
    }
}
