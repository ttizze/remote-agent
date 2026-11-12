import AgentCore
import AVFoundation
import PhotosUI
import SwiftUI
import UIKit

/// Media import
extension ComposerMediaPicker {
    func importPhotos(_ items: [PhotosPickerItem], draftKey: DraftIdentity) async {
        defer { preparingMedia = false }
        do {
            for item in items {
                guard model.draftKey == draftKey else { throw CocoaError(.userCancelled) }
                guard let media = try await item.loadTransferable(type: ChatMedia.self) else {
                    throw CocoaError(.fileReadUnknown)
                }
                defer {
                    try? FileManager.default.removeItem(at: media.url.deletingLastPathComponent())
                }
                guard model.draftKey == draftKey else { throw CocoaError(.userCancelled) }
                try await model.attach(media.url)
            }
        } catch {
            model.transferError = model.draftKey == draftKey ? error.localizedDescription
                : "チャットが切り替わったため、写真・動画をもう一度選択してください。"
        }
    }
}

extension ConversationComposer {
    func startDictation() {
        composerFocused = false
        model.transferError = nil
        model.notice = nil
        sendRecordedText = false
        let sendIntent = $sendRecordedText
        let key = model.draftKey
        var preparation: DictationPreparation?
        dictation.start(started: { [weak model] in
            preparation = model?.prepareDictation()
        }, completion: { [weak model] result in
            guard let model, model.draftKey == key else { return }
            switch result {
            case let .success(audio): model.transcribe(audio, draftKey: key, sendImmediately: sendIntent.wrappedValue,
                                                       preparation: preparation)
            case let .failure(error): model.transferError = error.localizedDescription
            }
        })
    }

    func openCamera() {
        composerFocused = false
        guard UIImagePickerController.isSourceTypeAvailable(.camera) else {
            model.transferError = "この端末ではカメラを利用できません。"
            return
        }
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: mediaPicker = .camera
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { granted in
                DispatchQueue.main.async {
                    if granted {
                        mediaPicker = .camera
                    } else {
                        model.transferError = "設定アプリでBexのカメラへのアクセスを許可してください。"
                    }
                }
            }
        default: model.transferError = "設定アプリでBexのカメラへのアクセスを許可してください。"
        }
    }
}
