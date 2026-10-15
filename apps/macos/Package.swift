// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "SkinOS",
    platforms: [.macOS(.v15)],
    products: [.executable(name: "SkinOS", targets: ["SkinOS"])],
    dependencies: [.package(url: "https://github.com/sparkle-project/Sparkle", exact: "2.10.0")],
    targets: [
        .executableTarget(name: "SkinOS", dependencies: [.product(name: "Sparkle", package: "Sparkle")],
            linkerSettings: [.unsafeFlags(["-Xlinker", "-rpath", "-Xlinker", "@executable_path/../Frameworks"])]),
        .testTarget(name: "SkinOSTests", dependencies: ["SkinOS"])
    ],
    swiftLanguageModes: [.v5]
)
