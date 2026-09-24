// swift-tools-version: 6.0
import PackageDescription

// The application's model — everything the screens ask that is not a
// widget — as a package, so `swift test` runs it natively on the macOS
// slice the way the library's tests run. Nothing in this target imports
// UIKit or SwiftUI; the screens live in `Sources/ChapbookApp`, which
// only `build.sh` compiles, because a `.app` is not something SwiftPM
// produces for iOS. The library it sits on is `../Chapbook`, and that one
// binds the XCFramework `../build-xcframework.sh` makes — run that first.
let package = Package(
    name: "ChapbookApp",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "ChapbookAppModel", targets: ["ChapbookAppModel"])
    ],
    dependencies: [
        .package(path: "../Chapbook")
    ],
    targets: [
        .target(
            name: "ChapbookAppModel",
            dependencies: [.product(name: "Chapbook", package: "Chapbook")]),
        .testTarget(name: "ChapbookAppModelTests", dependencies: ["ChapbookAppModel"]),
    ]
)
