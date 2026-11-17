// swift-tools-version: 6.0
// The Eve Link chat, native (docs/app-chat.md): eve's messages drawn in SwiftUI, with no web view.
//
//   EveChatCore  Foundation only: the daemon's lines as messages, the ledger that drops the long poll's copies of a
//                streamed turn, the send queue, and how a reply splits into markdown and cards.
//   EveChatUI    SwiftUI only (no UIKit, so it builds on the Mac too): bubbles, attachments, sources, Horizontal
//                History and the message list, over EveMarkdown and EveCards.
//
// The app (../../project.yml) depends on this package; the network, haptics, Quick Look and Safari stay in the app.
import PackageDescription

let package = Package(
    name: "EveChat",
    platforms: [.iOS(.v18), .macOS(.v15)],
    products: [
        .library(name: "EveChatCore", targets: ["EveChatCore"]),
        .library(name: "EveChatUI", targets: ["EveChatUI"]),
    ],
    dependencies: [
        .package(path: "../EveMarkdown"),
        .package(path: "../EveCards"),
        // The same pin as EveCards: a card tap's data arrives as json-render's JSONValue.
        .package(url: "https://github.com/vercel-labs/json-render", exact: "0.23.1"),
    ],
    targets: [
        .target(name: "EveChatCore"),
        .target(
            name: "EveChatUI",
            dependencies: [
                "EveChatCore",
                .product(name: "EveMarkdown", package: "EveMarkdown"),
                .product(name: "EveCardCore", package: "EveCards"),
                .product(name: "EveCardUI", package: "EveCards"),
                .product(name: "JSONRenderCore", package: "json-render"),
            ]
        ),
        .testTarget(name: "EveChatCoreTests", dependencies: ["EveChatCore"]),
    ]
)
