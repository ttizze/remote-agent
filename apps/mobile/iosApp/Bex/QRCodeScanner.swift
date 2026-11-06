import AVFoundation
import SwiftUI
import UIKit
import VisionKit

/// Pairing reads the Host's QR code through the system scanner.
struct QrScannerSheet: View {
    private enum CameraAccess {
        case ready, denied, restricted, unavailable, unknown
    }

    let completion: (String?) -> Void
    @State private var access: CameraAccess?

    var body: some View {
        NavigationStack {
            Group {
                switch access {
                case nil:
                    ProgressView()
                case .ready:
                    QrDataScanner { completion($0) }.ignoresSafeArea()
                case .denied:
                    unavailable("カメラの使用が許可されていません。設定でBexのカメラを許可し、読み取り画面を開き直してください。",
                                offersSettings: true)
                case .restricted:
                    unavailable("このiPhoneではカメラの使用が制限されています。")
                case .unavailable:
                    unavailable("カメラを使用できません。接続画面に戻り、接続情報を入力してください。")
                case .unknown:
                    unavailable("カメラのアクセス状態を確認できません。接続画面に戻り、接続情報を入力してください。")
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .navigationTitle("QRコードを読み取る")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("キャンセル") { completion(nil) }
                        .accessibilityIdentifier("scanner.cancel")
                }
            }
        }
        .accessibilityIdentifier("scanner.sheet")
        .task { access = await Self.cameraAccess() }
    }

    private func unavailable(_ message: String, offersSettings: Bool = false) -> some View {
        ContentUnavailableView {
            Image(systemName: "camera")
        } description: {
            AppText(message).accessibilityIdentifier("scanner.error")
        } actions: {
            if offersSettings, let url = URL(string: UIApplication.openSettingsURLString) {
                Button { UIApplication.shared.open(url) } label: { AppText("設定を開く") }
                    .accessibilityIdentifier("scanner.settings")
            }
        }
    }

    @MainActor private static func cameraAccess() async -> CameraAccess {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .notDetermined:
            let granted = await AVCaptureDevice.requestAccess(for: .video)
            return granted ? scannerAccess : .denied
        case .authorized:
            return scannerAccess
        case .denied:
            return .denied
        case .restricted:
            return .restricted
        @unknown default:
            return .unknown
        }
    }

    @MainActor private static var scannerAccess: CameraAccess {
        DataScannerViewController.isSupported && DataScannerViewController.isAvailable ? .ready : .unavailable
    }
}

private struct QrDataScanner: UIViewControllerRepresentable {
    let completion: (String) -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(completion: completion)
    }

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(recognizedDataTypes: [.barcode(symbologies: [.qr])],
                                                isHighlightingEnabled: true)
        scanner.delegate = context.coordinator
        return scanner
    }

    func updateUIViewController(_ scanner: DataScannerViewController, context _: Context) {
        if !scanner.isScanning {
            try? scanner.startScanning()
        }
    }

    static func dismantleUIViewController(_ scanner: DataScannerViewController, coordinator _: Coordinator) {
        scanner.stopScanning()
    }

    @MainActor final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        private let completion: (String) -> Void
        private var completed = false

        init(completion: @escaping (String) -> Void) {
            self.completion = completion
        }

        func dataScanner(_: DataScannerViewController, didAdd items: [RecognizedItem],
                         allItems _: [RecognizedItem]) {
            let code = items.lazy.compactMap { item -> String? in
                guard case let .barcode(barcode) = item else { return nil }
                return barcode.payloadStringValue
            }.first { !$0.isEmpty }
            guard !completed, let code else { return }
            completed = true
            completion(code)
        }
    }
}
