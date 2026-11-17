// Links in eve's replies: a bare URL in prose becomes a short labelled link ("Maps · Room 220"), and only the
// schemes the app opens stay links. Ported from the web chat's links.ts. Foundation only.
import Foundation

/// The schemes a reply's link may use. Anything else is drawn as plain text.
public enum MarkdownLinks {
    public static let schemes: Set<String> = ["http", "https", "mailto", "tel", "sms", "maps", "facetime", "facetime-audio"]

    /// The URL when its scheme is one the app opens, otherwise nil.
    public static func allowed(_ url: URL) -> URL? {
        guard let scheme = url.scheme?.lowercased(), schemes.contains(scheme) else { return nil }
        return url
    }
}

/// A short name for a URL: "Maps · Room 220", "YouTube", "docs.example.com › guide". A string that is not an
/// http or https URL with a host comes back as written.
public func linkLabel(_ raw: String) -> String {
    guard let parts = WebAddress(raw) else { return raw }
    let host = parts.host.hasPrefix("www.") ? String(parts.host.dropFirst(4)) : parts.host
    if (host == "google.com" && parts.path.hasPrefix("/maps")) || host == "maps.google.com" || host == "maps.apple.com" {
        guard let query = parts.query("query") ?? parts.query("q") ?? parts.query("daddr"), !query.isEmpty else {
            return "Maps"
        }
        return "Maps · " + String(query.replacingOccurrences(of: "+", with: " ").prefix(40))
    }
    if host == "youtube.com" || host == "youtu.be" { return "YouTube" }
    guard let first = parts.path.split(separator: "/").first.map(String.init) else { return host }
    // The web measures the segment as the browser encodes it, so a non-ASCII segment counts its escapes.
    let encoded = first.addingPercentEncoding(withAllowedCharacters: WebAddress.segmentCharacters) ?? first
    guard encoded.utf16.count <= 24 else { return host }
    // A segment that is not valid percent-encoding ("50%off") is shown as written.
    return host + " › " + (first.removingPercentEncoding ?? first)
}

/// Bare URLs in prose become short labelled markdown links. A URL already inside a markdown link, an <angle> link
/// or code is left alone. A code fence that is still open counts as code to the end of the text.
public func compactLinks(_ markdown: String) -> String {
    guard markdown.contains("http") else { return markdown }
    let ns = markdown as NSString
    var out = ""
    var last = 0
    for match in LinkPatterns.protected.matches(in: markdown, range: NSRange(location: 0, length: ns.length)) {
        out += labelBareLinks(ns.substring(with: NSRange(location: last, length: match.range.location - last)))
        out += ns.substring(with: match.range)
        last = NSMaxRange(match.range)
    }
    return out + labelBareLinks(ns.substring(from: last))
}

private func labelBareLinks(_ prose: String) -> String {
    guard prose.contains("http") else { return prose }
    let ns = prose as NSString
    var out = ""
    var last = 0
    for match in LinkPatterns.bare.matches(in: prose, range: NSRange(location: 0, length: ns.length)) {
        let url = ns.substring(with: match.range(at: 2))
        let label = linkLabel(url).replacingOccurrences(of: "[", with: "").replacingOccurrences(of: "]", with: "")
        out += ns.substring(with: NSRange(location: last, length: match.range.location - last))
        out += ns.substring(with: match.range(at: 1)) + "[" + label + "](" + url + ")"
        last = NSMaxRange(match.range)
    }
    return out + ns.substring(from: last)
}

private enum LinkPatterns {
    /// Code fences, code spans, markdown links and <angle> links: text the rewrite must not touch.
    static let protected = try! NSRegularExpression(
        pattern: #"```[\s\S]*?(?:```|\z)|`[^`\n]*`|\[[^\]\n]*\]\([^)\s]*\)|<https?:[^>\s]+>"#)
    /// A bare URL at the start of the prose or after a space or "(", without its trailing punctuation.
    static let bare = try! NSRegularExpression(
        pattern: #"(\A|[\s(])(https?://[^\s<>()\]]+[^\s<>()\].,;:!?'"])"#)
}

/// The parts of an http or https address that a label needs, split by hand so an odd path is kept as written.
private struct WebAddress {
    var host: String
    var path: String
    var queryText: String

    static let segmentCharacters: CharacterSet = {
        var set = CharacterSet.urlPathAllowed
        set.insert("%")
        return set
    }()

    init?(_ raw: String) {
        let lower = raw.lowercased()
        guard let prefix = ["https://", "http://"].first(where: lower.hasPrefix) else { return nil }
        var rest = raw.dropFirst(prefix.count)
        if let hash = rest.firstIndex(of: "#") { rest = rest[..<hash] }
        var query: Substring = ""
        if let mark = rest.firstIndex(of: "?") {
            query = rest[rest.index(after: mark)...]
            rest = rest[..<mark]
        }
        let slash = rest.firstIndex(of: "/") ?? rest.endIndex
        var authority = rest[..<slash]
        if let at = authority.lastIndex(of: "@") { authority = authority[authority.index(after: at)...] }
        if let colon = authority.lastIndex(of: ":"), !authority.hasSuffix("]"),
           authority[authority.index(after: colon)...].allSatisfy(\.isNumber) {
            authority = authority[..<colon]
        }
        guard !authority.isEmpty else { return nil }
        host = authority.lowercased()
        path = slash == rest.endIndex ? "/" : String(rest[slash...])
        queryText = String(query)
    }

    /// The first value of a query name, decoded as a form value ("+" is a space).
    func query(_ name: String) -> String? {
        for pair in queryText.split(separator: "&") {
            let bits = pair.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            let key = String(bits[0]).replacingOccurrences(of: "+", with: " ")
            guard (key.removingPercentEncoding ?? key) == name else { continue }
            let value = bits.count > 1 ? String(bits[1]).replacingOccurrences(of: "+", with: " ") : ""
            return value.removingPercentEncoding ?? value
        }
        return nil
    }
}
