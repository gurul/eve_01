// The pairing (eve's tunnel address and the link token) and where it is kept. Shared by the app and its widget
// extension (docs/widget.md): the home-screen widget reads eve with the same token, so the Keychain item lives in
// an access group both targets list (project.yml, keychain-access-groups).
import Foundation
import Security

struct Pairing: Equatable, Sendable {
    var base: URL
    var token: String

    /// evelink://pair?u=<https origin>&t=<token>, from the pairing page (stick_link.PAIR_PAGE).
    init?(link: URL) {
        guard link.scheme == "evelink", link.host == "pair",
              let items = URLComponents(url: link, resolvingAgainstBaseURL: false)?.queryItems,
              let u = items.first(where: { $0.name == "u" })?.value, let base = URL(string: u),
              base.scheme == "https" || base.host == "127.0.0.1",
              let t = items.first(where: { $0.name == "t" })?.value, t.count >= 32 else { return nil }
        self.base = base
        self.token = t
    }

    init(base: URL, token: String) {
        self.base = base
        self.token = token
    }
}

/// The pairing, in the Keychain (the token is a password to eve).
enum Keychain {
    private static let service = "com.github.eve-bridge.EveLink"
    private static let account = "pairing"

    /// The access group the app and the widget share: "<team prefix>com.github.eve-bridge.EveLink.shared",
    /// from each target's Info.plist (EveKeychainGroup, filled in at build time). Nil where a build has none (the
    /// Mac tests): the item then stays in the app's own group, as before the widget.
    static let sharedGroup: String? = {
        guard let g = Bundle.main.object(forInfoDictionaryKey: "EveKeychainGroup") as? String,
              !g.isEmpty, !g.contains("$(") else { return nil }
        return g
    }()

    private static var base: [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service,
         kSecAttrAccount as String: account]
    }

    static func save(_ p: Pairing) {
        let data = Data("\(p.base.absoluteString)\n\(p.token)".utf8)
        SecItemDelete(base as CFDictionary)        // every copy this target can see: its own group's and the shared one
        var add = base
        add[kSecValueData as String] = data
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly   // locked-phone calls
        if let group = sharedGroup {
            var shared = add
            shared[kSecAttrAccessGroup as String] = group
            if SecItemAdd(shared as CFDictionary, nil) == errSecSuccess { return }
            // a build signed without the entitlement: keep the pairing where the app alone reads it, as before
        }
        SecItemAdd(add as CFDictionary, nil)
    }

    static func load() -> Pairing? {
        var query = base
        query[kSecReturnData as String] = true
        var out: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &out) == errSecSuccess, let data = out as? Data else { return nil }
        let parts = String(decoding: data, as: UTF8.self).split(separator: "\n", maxSplits: 1).map(String.init)
        guard parts.count == 2, let base = URL(string: parts[0]) else { return nil }
        return Pairing(base: base, token: parts[1])
    }

    /// A pairing made before the widget lives in the app's own group, where the widget cannot read it: copy it into
    /// the shared group once (the app calls this at launch). Nothing to do when it is there already.
    static func share(_ p: Pairing) {
        guard let group = sharedGroup else { return }
        var query = base
        query[kSecAttrAccessGroup as String] = group
        if SecItemCopyMatching(query as CFDictionary, nil) == errSecSuccess { return }
        save(p)
    }
}
