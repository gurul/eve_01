// swift-tools-version: 6.0
// eve's replies as native text (docs/app-chat.md): markdown parsed into blocks and drawn in SwiftUI, with no web view.
// SwiftUI and Foundation only (no UIKit), so it builds and tests on the Mac with `swift test`.
import PackageDescription

let package = Package(
    name: "EveMarkdown",
    platforms: [.iOS(.v18), .macOS(.v15)],
    products: [.library(name: "EveMarkdown", targets: ["EveMarkdown"])],
    targets: [
        .target(name: "EveMarkdown"),
        .testTarget(name: "EveMarkdownTests", dependencies: ["EveMarkdown"]),
    ]
)
