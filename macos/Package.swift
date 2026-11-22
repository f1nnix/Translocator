// swift-tools-version: 6.2
// The native macOS shell of Translocator. Run `macos/build-core.sh` first:
// it builds the Rust core into build/TLCoreFFI.xcframework and writes the
// Swift bindings to Sources/TLCore/TLCore.swift.
import PackageDescription

let package = Package(
    name: "Translocator",
    platforms: [.macOS(.v26)],
    products: [
        .executable(name: "Translocator", targets: ["Translocator"]),
    ],
    targets: [
        // The Rust core as a static library, with its C header and module map.
        .binaryTarget(name: "TLCoreFFI", path: "build/TLCoreFFI.xcframework"),
        // The generated UniFFI bindings over it.
        .target(
            name: "TLCore",
            dependencies: ["TLCoreFFI"],
            path: "Sources/TLCore",
            linkerSettings: [
                .linkedFramework("Security"),
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("CoreFoundation"),
                .linkedFramework("CoreServices"),
                .linkedLibrary("iconv"),
            ]
        ),
        .executableTarget(
            name: "Translocator",
            dependencies: ["TLCore"],
            path: "Sources/Translocator"
        ),
    ],
    swiftLanguageModes: [.v6]
)
