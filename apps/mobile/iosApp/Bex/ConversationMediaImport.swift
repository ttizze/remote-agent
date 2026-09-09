import AgentCore
import AVFoundation
import SwiftUI
import UniformTypeIdentifiers

/// Media import
extension ThreadScreen {
    func importPhotos(_ providers: ArraySlice<NSItemProvider>, draftKey: String) {
        guard model.draftKey == draftKey else {
            preparingMedia = false
            model.transferError = "チャットが切り替わったため、写真・動画をもう一度選択してください。"
            return
        }
        guard let provider = providers.first else { preparingMedia = false; return }
        let type = provider.hasItemConformingToTypeIdentifier(UTType.movie.identifier) ? UTType.movie : UTType.image
        provider.loadFileRepresentation(forTypeIdentifier: type.identifier) { url, error in
            let result = Result {
                guard let url else { throw error ?? CocoaError(.fileReadUnknown) }
                return try retainChatMedia(url)
            }
            DispatchQueue.main.async {
                guard model.draftKey == draftKey else {
                    if case let .success(url) = result {
                        try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
                    }
                    preparingMedia = false
                    model.transferError = "チャットが切り替わったため、写真・動画をもう一度選択してください。"
                    return
                }
                switch result {
                case let .success(url):
                    model.attach(url, temporaryDirectory: url.deletingLastPathComponent()) {
                        if model.transferError != nil {
                            preparingMedia = false; return
                        }
                        importPhotos(providers.dropFirst(), draftKey: draftKey)
                    }
                case let .failure(error):
                    preparingMedia = false
                    model.transferError = error.localizedDescription
                }
            }
        }
    }

    func finishMediaImport(_ result: Result<URL?, Error>) {
        preparingMedia = false
        switch result {
        case let .success(url):
            if let url {
                model.attach(url, temporaryDirectory: url.deletingLastPathComponent())
            }
        case let .failure(error): model.transferError = error.localizedDescription
        }
    }

    func openCamera() {
        composerFocused = false
        guard UIImagePickerController.isSourceTypeAvailable(.camera) else {
            model.transferError = "この端末ではカメラを利用できません。"
            return
        }
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: showingCamera = true
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { granted in
                DispatchQueue.main.async {
                    if granted {
                        showingCamera = true
                    } else {
                        model.transferError = "設定アプリでBexのカメラへのアクセスを許可してください。"
                    }
                }
            }
        default: model.transferError = "設定アプリでBexのカメラへのアクセスを許可してください。"
        }
    }
}
