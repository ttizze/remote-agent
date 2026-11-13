// swift-tools-version:6.2
import PackageDescription

let package = Package(
    name: "NativeUI",
    platforms: [.macOS("26.0"), .iOS("26.0")],
    products: [.library(name: "NativeUI", targets: ["NativeUI"])],
    targets: [.target(name: "NativeUI")],
    swiftLanguageModes: [.v6]
)
