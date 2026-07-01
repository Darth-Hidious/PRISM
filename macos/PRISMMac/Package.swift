// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "PRISMMac",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "PRISMMac", targets: ["PRISMMac"])
    ],
    targets: [
        .executableTarget(name: "PRISMMac")
    ]
)

