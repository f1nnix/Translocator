// swift-tools-version: 6.2
// The native macOS shell of Translocator. Run `macos/build-core.sh` first:
// it builds the Rust core into build/TLCoreFFI.xcframework and writes the
// Swift bindings to Sources/Translocator/Core/TLCore.swift.
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
        // The app, with the generated UniFFI bindings in Core/. They are not
        // a target of their own: Xcode 26's build system cannot link a
        // library target over a binary one (`library not found for -ltl_ffi`).
        .executableTarget(
            name: "Translocator",
            dependencies: ["TLCoreFFI"],
            path: "Sources/Translocator",
            linkerSettings: [
                .linkedFramework("Security"),
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("CoreFoundation"),
                .linkedFramework("CoreServices"),
                .linkedLibrary("iconv"),
            ]
        ),
    ],
    swiftLanguageModes: [.v6]
)
