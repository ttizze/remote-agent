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
                "AgentsSheet.swift",
                "AppPresentationData.swift",
                "AppViewActions.swift",
                "AppViewModel.swift",
                "ArchivedScreen.swift",
                "Assets.xcassets",
                "BexIOSApp.swift",
                "Composer.swift",
                "ComposerAttachments.swift",
                "ComposerMenu.swift",
                "ConversationMarkdown.swift",
                "FeedDividers.swift",
                "FeedRows.swift",
                "Info.plist",
                "NewTaskFlow.swift",
                "PairingScreens.swift",
                "PlatformServices.swift",
                "PrivacyInfo.xcprivacy",
                "QRCodeCaptureViewController.swift",
                "QueueSheet.swift",
                "RequestCards.swift",
                "ReviewScreen.swift",
                "SettingsScreen.swift",
                "SetupCard.swift",
                "SnoozeSheet.swift",
                "SwiftUIRoot.swift",
                "TerminalScreen.swift",
                "Theme.swift",
                "ThreadListHeader.swift",
                "ThreadListRows.swift",
                "ThreadListScreen.swift",
                "ThreadPresentation.swift",
                "ThreadScreen.swift",
                "ThreadSettingsSheet.swift",
                "WorkLogRows.swift",
                "WorkingControl.swift",
                "WorkspaceFileEditor.swift",
                "WorkspaceScreen.swift",
                "WorkspaceViews.swift",
                "WorktreeSettings.swift"
            ],
            sources: ["DraftRevision.swift", "ImageSizing.swift"]
        ),
        .testTarget(name: "BexNativeStateTests", dependencies: ["BexNativeState"], path: "UnitTests")
    ]
)
