# Use a Rust host, shared Kotlin logic, and native mobile UIs

Remote Agent implements the security-sensitive Host Daemon and Mobile Client transport in Rust. Android and iOS share Kotlin application state, reconciliation, cache rules, and Codex-only DTOs. Android renders that state with Jetpack Compose and iOS renders it with SwiftUI through one iOS-specific Snapshot/intent interface. Operating-system facilities such as secure storage, QR capture, file picking, lifecycle handling, and initially QUIC remain platform-specific implementations behind Kotlin interfaces.
