// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Luch",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "luch", targets: ["luch"]),
        .executable(name: "LuchApp", targets: ["LuchApp"]),
        .executable(name: "luchctl", targets: ["luchctl"]),
        .library(name: "LuchEngine", targets: ["LuchEngine"]),
        // Loaded into the system perl, not linked into anything (NowPlaying.swift says why).
        .library(name: "LuchNowPlaying", type: .dynamic, targets: ["LuchNowPlaying"]),
    ],
    targets: [
        .systemLibrary(name: "COpus", path: "Sources/COpus", pkgConfig: "opus", providers: [.brew(["opus"])]),
        .target(name: "LuchProto", path: "Sources/LuchProto"),
        .target(name: "LuchNet", dependencies: ["LuchProto"], path: "Sources/LuchNet"),
        .target(name: "CVirtualDisplay", path: "Sources/CVirtualDisplay",
                linkerSettings: [.linkedFramework("CoreGraphics"), .linkedFramework("Foundation")]),
        .target(name: "LuchMedia", dependencies: ["LuchProto", "COpus", "CVirtualDisplay"], path: "Sources/LuchMedia"),
        .target(name: "LuchEngine", dependencies: ["LuchProto", "LuchNet", "LuchMedia"], path: "Sources/LuchEngine"),
        .target(name: "LuchNowPlaying", path: "Sources/LuchNowPlaying", linkerSettings: [.linkedFramework("AppKit")]),
        .target(name: "LuchLocal", dependencies: ["LuchProto"], path: "Sources/LuchLocal"),
        .target(name: "LuchCast", dependencies: ["LuchProto", "LuchNet", "LuchEngine"], path: "Sources/LuchCast"),
        .target(name: "LuchPresent", dependencies: ["LuchProto"], path: "Sources/LuchPresent",
                linkerSettings: [.linkedFramework("PDFKit"), .linkedFramework("AppKit")]),
        .executableTarget(name: "luch", dependencies: ["LuchProto", "LuchNet", "LuchMedia", "LuchEngine"], path: "Sources/luch"),
        .executableTarget(name: "LuchApp", dependencies: ["LuchProto", "LuchNet", "LuchMedia", "LuchEngine", "LuchLocal", "LuchCast", "LuchPresent"], path: "Sources/LuchApp"),
        .executableTarget(name: "luchctl", dependencies: ["LuchLocal"], path: "Sources/luchctl"),
        .testTarget(name: "LuchTests", dependencies: ["LuchProto", "LuchNet", "LuchMedia", "LuchEngine", "LuchCast", "LuchLocal", "LuchPresent"], path: "Tests/LuchTests"),
    ]
)
