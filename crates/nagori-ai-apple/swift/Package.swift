// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "nagori_apple",
    platforms: [
        // Must match the crate's `SwiftLinker::new("26.0")` and the app's
        // `minimumSystemVersion`; the default (10.13) lets `import
        // FoundationModels` link but fail to load at runtime.
        .macOS("26.0")
    ],
    products: [
        .library(name: "nagori_apple", type: .static, targets: ["nagori_apple"])
    ],
    dependencies: [
        .package(url: "https://github.com/Brendonovich/swift-rs", from: "1.0.6")
    ],
    targets: [
        .target(
            name: "nagori_apple",
            dependencies: [
                .product(name: "SwiftRs", package: "swift-rs")
            ],
            path: "Sources/nagori_apple"
        )
    ]
)
