// swift-tools-version:5.9
import PackageDescription

// Notch for macOS. `NotchCore` holds everything that can be tested without a screen
// (session model, hook and transcript parsing, the hooks installer); `Notch` is the app.
let package = Package(
    name: "Notch",
    platforms: [.macOS(.v13)],
    targets: [
        .target(name: "NotchCore", path: "Sources/NotchCore", linkerSettings: [.linkedLibrary("sqlite3")]),
        .executableTarget(name: "Notch", dependencies: ["NotchCore"], path: "Sources/Notch"),
        .testTarget(name: "NotchCoreTests", dependencies: ["NotchCore"], path: "Tests/NotchCoreTests"),
    ]
)
