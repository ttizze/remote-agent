// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "BexNativeState",
    products: [.library(name: "BexNativeState", targets: ["BexNativeState"])],
    targets: [
        .target(
            name: "BexNativeState",
            path: "Bex",
            exclude: [
                "AccountViews.swift",
                "AgentSettings.swift",
                "AppPresentationData.swift",
                "AppViewActions.swift",
                "AppViewModel.swift",
                "Assets.xcassets",
                "BexIOSApp.swift",
                "ConversationComposer.swift",
                "ConversationMarkdown.swift",
                "ConversationRequest.swift",
                "ConversationRows.swift",
                "ConversationScreen.swift",
                "Info.plist",
                "PlatformServices.swift",
                "PrivacyInfo.xcprivacy",
                "QRCodeCaptureViewController.swift",
                "SwiftUIRoot.swift",
                "TaskList.swift",
                "TerminalScreen.swift",
                "WorkspaceFileEditor.swift",
                "WorkspaceScreen.swift",
                "WorkspaceViews.swift",
                "WorktreeSettings.swift"
            ],
            sources: ["DraftRevision.swift"]
        ),
        .testTarget(name: "BexNativeStateTests", dependencies: ["BexNativeState"], path: "UnitTests")
    ]
)
