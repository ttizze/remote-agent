import AVFoundation
import UIKit

enum BexQrCaptureError: LocalizedError {
    case cameraUnavailable
    case permissionDenied
    case sessionConfigurationFailed

    var errorDescription: String? {
        switch self {
        case .cameraUnavailable: "This device has no camera available for QR scanning."
        case .permissionDenied: "Camera access is required to scan the pairing QR code."
        case .sessionConfigurationFailed: "The QR scanner could not be configured."
        }
    }
}

final class BexQrCaptureViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    private let completion: (Result<String, BexQrCaptureError>) -> Void
    private let session = AVCaptureSession()
    private let previewLayer = AVCaptureVideoPreviewLayer()
    private var hasCompleted = false

    init(completion: @escaping (Result<String, BexQrCaptureError>) -> Void) {
        self.completion = completion
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder _: NSCoder) {
        nil
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        previewLayer.session = session
        previewLayer.videoGravity = .resizeAspectFill
        view.layer.addSublayer(previewLayer)
        requestCameraAndConfigure()
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        previewLayer.frame = view.bounds
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        session.stopRunning()
    }

    private func requestCameraAndConfigure() {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            configureAndStart()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                DispatchQueue.main.async {
                    guard let self else { return }
                    if granted {
                        self.configureAndStart()
                    } else {
                        self.finish(.failure(.permissionDenied))
                    }
                }
            }
        case .denied, .restricted:
            finish(.failure(.permissionDenied))
        @unknown default:
            finish(.failure(.permissionDenied))
        }
    }

    private func configureAndStart() {
        guard let camera = AVCaptureDevice.default(for: .video) else {
            finish(.failure(.cameraUnavailable))
            return
        }
        session.beginConfiguration()
        do {
            let input = try AVCaptureDeviceInput(device: camera)
            guard session.canAddInput(input) else { throw BexQrCaptureError.sessionConfigurationFailed }
            let output = AVCaptureMetadataOutput()
            guard session.canAddOutput(output) else { throw BexQrCaptureError.sessionConfigurationFailed }
            session.addInput(input)
            session.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: .main)
            output.metadataObjectTypes = [.qr]
            session.commitConfiguration()
            DispatchQueue.global(qos: .userInitiated).async { [weak session] in session?.startRunning() }
        } catch {
            session.commitConfiguration()
            finish(.failure(.sessionConfigurationFailed))
        }
    }

    func metadataOutput(
        _: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from _: AVCaptureConnection
    ) {
        guard
            !hasCompleted,
            let code = metadataObjects.compactMap({ $0 as? AVMetadataMachineReadableCodeObject })
            .first?.stringValue,
            !code.isEmpty
        else { return }
        finish(.success(code))
    }

    private func finish(_ result: Result<String, BexQrCaptureError>) {
        guard !hasCompleted else { return }
        hasCompleted = true
        session.stopRunning()
        dismiss(animated: true) { self.completion(result) }
    }
}
