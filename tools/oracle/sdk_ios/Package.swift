// swift-tools-version:5.9
// The iOS SDK harness (README in tools/oracle/README.md): firebase-ios-sdk's FirebaseFirestore,
// built for macOS, which runs the same client as iOS over the same gRPC core.
import PackageDescription

let package = Package(
    name: "sdk_ios",
    platforms: [.macOS(.v13)],
    dependencies: [
        .package(url: "https://github.com/firebase/firebase-ios-sdk.git", exact: "13.0.1"),
    ],
    targets: [
        .executableTarget(
            name: "sdk_ios",
            dependencies: [.product(name: "FirebaseFirestore", package: "firebase-ios-sdk")]
        ),
    ]
)
