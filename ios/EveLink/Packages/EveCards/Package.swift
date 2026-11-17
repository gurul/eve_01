// swift-tools-version: 6.0
// eve's cards, native (docs/app-chat.md, "The card engine"): a json-render block in a reply, checked against eve's
// catalog and drawn in SwiftUI by json-render's Swift renderer (github.com/vercel-labs/json-render).
//
//   EveCardCore  Foundation and JSONRenderCore: a block's lines to a spec, the checks a finished card must pass, and
//                its readable text. The same rules as the web page's specProgram.ts.
//   EveCardUI    SwiftUI only (no UIKit, so it builds on the Mac): the catalog's components and the card around them.
import PackageDescription

let package = Package(
    name: "EveCards",
    platforms: [.iOS(.v18), .macOS(.v15)],
    products: [
        .library(name: "EveCardCore", targets: ["EveCardCore"]),
        .library(name: "EveCardUI", targets: ["EveCardUI"]),
    ],
    dependencies: [
        // json-render for Swift shipped in 0.23.0 (2026-11-15); the repository root is the package from 0.23.1.
        .package(url: "https://github.com/vercel-labs/json-render", exact: "0.23.1"),
    ],
    targets: [
        .target(
            name: "EveCardCore",
            dependencies: [.product(name: "JSONRenderCore", package: "json-render")],
            resources: [.process("Resources")]
        ),
        .target(
            name: "EveCardUI",
            dependencies: [
                "EveCardCore",
                .product(name: "JSONRenderCore", package: "json-render"),
                .product(name: "JSONRenderSwiftUI", package: "json-render"),
            ]
        ),
        .testTarget(name: "EveCardCoreTests", dependencies: ["EveCardCore"], resources: [.copy("Fixtures")]),
        .testTarget(
            name: "EveCardUITests",
            dependencies: [
                "EveCardUI",
                "EveCardCore",
                .product(name: "JSONRenderCore", package: "json-render"),
                .product(name: "JSONRenderSwiftUI", package: "json-render"),
            ],
            // the parity fixture's blocks, drawn: one file, read by both test targets
            resources: [.copy("../EveCardCoreTests/Fixtures/card-parity.json")]
        ),
    ]
)
