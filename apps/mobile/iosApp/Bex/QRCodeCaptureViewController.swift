import AgentCore
import AVFoundation
import UIKit

final class QrCaptureViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    private let language: UiLanguage
    private let completion: (String) -> Void
    private let session = AVCaptureSession()
    private let sessionQueue = DispatchQueue(label: "agent.qr-capture")
    private let previewLayer = AVCaptureVideoPreviewLayer()
    private var hasCompleted = false

    init(language: UiLanguage, completion: @escaping (String) -> Void) {
        self.language = language
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
        hasCompleted = true
        sessionQueue.async { [session] in session.stopRunning() }
    }

    private func requestCameraAndConfigure() {
        let authorization = AVCaptureDevice.authorizationStatus(for: .video)
        switch authorization {
        case .authorized, .denied:
            configureAndStart(authorized: authorization == .authorized)
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                DispatchQueue.main.async {
                    guard let self, !self.hasCompleted else { return }
                    self.configureAndStart(authorized: granted)
                }
            }
        case .restricted:
            showCameraError("このiPhoneではカメラの使用が制限されています。")
        @unknown default:
            showCameraError("カメラのアクセス状態を確認できません。接続画面に戻り、接続情報を入力してください。")
        }
    }

    private func configureAndStart(authorized: Bool) {
        guard authorized else {
            showCameraError("カメラの使用が許可されていません。設定でBexのカメラを許可し、読み取り画面を開き直してください。", offersSettings: true)
            return
        }
        guard let camera = AVCaptureDevice.default(for: .video) else {
            showCameraError("カメラを使用できません。接続画面に戻り、接続情報を入力してください。")
            return
        }
        session.beginConfiguration()
        let output = AVCaptureMetadataOutput()
        guard let input = try? AVCaptureDeviceInput(device: camera),
              session.canAddInput(input), session.canAddOutput(output)
        else {
            session.commitConfiguration()
            showCameraError("カメラを起動できません。接続画面に戻り、接続情報を入力してください。")
            return
        }
        session.addInput(input)
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]
        session.commitConfiguration()
        sessionQueue.async { [session] in session.startRunning() }
    }

    private func showCameraError(_ message: String, offersSettings: Bool = false) {
        let label = UILabel()
        label.text = localizedUiText(language: language, source: message)
        label.textColor = .white
        label.font = .preferredFont(forTextStyle: .body)
        label.adjustsFontForContentSizeCategory = true
        label.numberOfLines = 0
        label.textAlignment = .center
        label.accessibilityIdentifier = "scanner.error"
        let stack = UIStackView(arrangedSubviews: [label])
        stack.axis = .vertical
        stack.spacing = 20
        if offersSettings {
            let settings = UIButton(type: .system)
            settings.setTitle(localizedUiText(language: language, source: "設定を開く"), for: .normal)
            settings.accessibilityIdentifier = "scanner.settings"
            settings.addTarget(self, action: #selector(openSettings), for: .touchUpInside)
            stack.addArrangedSubview(settings)
        }
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor, constant: -24),
            stack.centerYAnchor.constraint(equalTo: view.safeAreaLayoutGuide.centerYAnchor)
        ])
    }

    @objc private func openSettings() {
        if let url = URL(string: UIApplication.openSettingsURLString) {
            UIApplication.shared.open(url)
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
        hasCompleted = true
        completion(code)
    }
}
