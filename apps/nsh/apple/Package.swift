// swift-tools-version:6.2
import PackageDescription

let package = Package(
    name: "BexNshNative",
    platforms: [.macOS("26.0"), .iOS("26.0")],
    products: [.library(name: "BexNshNative", type: .dynamic, targets: ["BexNshNative"])],
    dependencies: [
        .package(path: "../../native-ui"),
        .package(path: "../../native-browser/apple-views"),
        .package(path: "../../native-settings"),
        .package(url: "https://github.com/migueldeicaza/SwiftTerm.git", exact: "1.99.1")
    ],
    targets: [
        .target(
            name: "BexNshNative",
            dependencies: [
                .product(name: "NativeUI", package: "native-ui"),
                .product(name: "SwiftTerm", package: "SwiftTerm"),
                .product(name: "NativeBrowserViews", package: "apple-views"),
                .product(name: "NativeSettingsViews", package: "native-settings")
            ]
        ),
        .testTarget(name: "BexNshNativeTests", dependencies: ["BexNshNative"], resources: [.process("Fixtures")])
    ],
    swiftLanguageModes: [.v6]
)
