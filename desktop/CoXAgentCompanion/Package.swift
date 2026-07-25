// swift-tools-version: 5.9
// CoXAgent menu-bar companion: a tiny SwiftUI status app for macOS.
// Full dashboard stays in the desktop shell / web — this only shows hub
// health at a glance and offers quick actions.
//
// Layout mirrors the hexagonal split used everywhere else in the repo:
//   CompanionCore = domain + application (pure, unit-tested, no UI)
//   CompanionApp  = presentation (SwiftUI MenuBarExtra) + composition root
import PackageDescription

let package = Package(
    name: "CoXAgentCompanion",
    platforms: [.macOS(.v13)],
    targets: [
        .target(name: "CompanionCore"),
        .executableTarget(
            name: "CompanionApp",
            dependencies: ["CompanionCore"]
        ),
        .testTarget(
            name: "CompanionCoreTests",
            dependencies: ["CompanionCore"]
        ),
    ]
)
