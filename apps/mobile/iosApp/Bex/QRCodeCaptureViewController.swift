import AVFoundation
import UIKit

final class BexQrCaptureViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    private let completion: (String?) -> Void
    private let session = AVCaptureSession()
    private let previewLayer = AVCaptureVideoPreviewLayer()
    private var hasCompleted = false

    init(completion: @escaping (String?) -> Void) {
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
                        self.finish(nil)
                    }
                }
            }
        default:
            finish(nil)
        }
    }

    private func configureAndStart() {
        guard let camera = AVCaptureDevice.default(for: .video) else {
            finish(nil)
            return
        }
        session.beginConfiguration()
        let output = AVCaptureMetadataOutput()
        guard let input = try? AVCaptureDeviceInput(device: camera),
              session.canAddInput(input), session.canAddOutput(output)
        else {
            session.commitConfiguration()
            finish(nil)
            return
        }
        session.addInput(input)
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]
        session.commitConfiguration()
        DispatchQueue.global(qos: .userInitiated).async { [weak session] in session?.startRunning() }
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
        finish(code)
    }

    private func finish(_ result: String?) {
        guard !hasCompleted else { return }
        hasCompleted = true
        session.stopRunning()
        dismiss(animated: true) { self.completion(result) }
    }
}
