// A JSON value that remembers the order its keys were written in, and the reader that makes one from text.
// The web page checks a card with JavaScript objects (specProgram.ts), whose key order decides the order orphan
// elements are shown in, so the native check keeps the same order. Foundation's JSON reader does not.
// Behaviour: docs/app-chat.md, "The card engine".
import Foundation
import JSONRenderCore

/// One JSON value. An object keeps its keys in the order JavaScript would list them.
indirect enum CardValue: Equatable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([CardValue])
    case object(CardObject)

    var objectValue: CardObject? {
        if case .object(let value) = self { return value }
        return nil
    }

    /// The items of an array, or nothing for any other value (the web's `array()`).
    var items: [CardValue] {
        if case .array(let value) = self { return value }
        return []
    }

    var stringValue: String? {
        if case .string(let value) = self { return value }
        return nil
    }

    /// A string, number or boolean as text, or "" for anything else (the web's `scalar()`).
    var scalarText: String {
        switch self {
        case .string(let value): return value
        case .number(let value): return jsNumberText(value)
        case .bool(let value): return value ? "true" : "false"
        default: return ""
        }
    }

    /// JavaScript truthiness.
    var isTruthy: Bool {
        switch self {
        case .null: return false
        case .bool(let value): return value
        case .number(let value): return value != 0 && !value.isNaN
        case .string(let value): return !value.isEmpty
        case .array, .object: return true
        }
    }

    /// An object with a key that starts with "$": a value json-render resolves when it draws.
    var isDynamic: Bool {
        guard case .object(let object) = self else { return false }
        return object.keys.contains { $0.hasPrefix("$") }
    }

    /// The same value for json-render's Swift package.
    var jsonValue: JSONValue {
        switch self {
        case .null: return .null
        case .bool(let value): return .bool(value)
        case .number(let value): return .number(value)
        case .string(let value): return .string(value)
        case .array(let value): return .array(value.map(\.jsonValue))
        case .object(let value):
            var out: [String: JSONValue] = [:]
            for key in value.keys { out[key] = value[key]?.jsonValue }
            return .object(out)
        }
    }
}

/// A JSON object. Two objects are equal when they hold the same members, in any order.
struct CardObject: Equatable, Sendable {
    private var written: [String] = []
    private var members: [String: CardValue] = [:]

    init() {}

    init(_ pairs: [(String, CardValue)]) {
        for (key, value) in pairs { self[key] = value }
    }

    var isEmpty: Bool { members.isEmpty }
    var count: Int { members.count }

    func has(_ key: String) -> Bool { members[key] != nil }

    subscript(key: String) -> CardValue? {
        get { members[key] }
        set {
            if let newValue {
                if members.updateValue(newValue, forKey: key) == nil { written.append(key) }
            } else if members.removeValue(forKey: key) != nil {
                written.removeAll { $0 == key }
            }
        }
    }

    /// The keys as `Object.keys` lists them: whole-number keys first, smallest first, then the rest as written.
    var keys: [String] {
        var indexes: [(UInt32, String)] = []
        var names: [String] = []
        for key in written {
            if let index = arrayIndex(key) { indexes.append((index, key)) } else { names.append(key) }
        }
        if indexes.isEmpty { return names }
        return indexes.sorted { $0.0 < $1.0 }.map(\.1) + names
    }

    static func == (a: CardObject, b: CardObject) -> Bool { a.members == b.members }

    /// A key JavaScript treats as an array index: "0", or digits with no leading zero, below 2^32 - 1.
    private func arrayIndex(_ key: String) -> UInt32? {
        let bytes = Array(key.utf8)
        guard !bytes.isEmpty, bytes.count <= 10, bytes.allSatisfy({ $0 >= 48 && $0 <= 57 }) else { return nil }
        if bytes.count > 1 && bytes[0] == 48 { return nil }
        guard let value = UInt64(key), value < 4_294_967_295 else { return nil }
        return UInt32(value)
    }
}

// MARK: - JavaScript's text rules

/// `String(number)` in JavaScript: 20, 7.5, 1e+21, 1e-7.
func jsNumberText(_ value: Double) -> String {
    if value.isNaN { return "NaN" }
    if value.isInfinite { return value < 0 ? "-Infinity" : "Infinity" }
    if value == 0 { return "0" }
    // Swift prints the shortest digits that read back as the same number, as JavaScript does; only the layout differs.
    var text = "\(abs(value))"
    var exponent = 0
    if let e = text.firstIndex(where: { $0 == "e" || $0 == "E" }) {
        exponent = Int(text[text.index(after: e)...]) ?? 0
        text = String(text[..<e])
    }
    var digits = ""
    var point = text.count
    for (i, ch) in text.enumerated() {
        if ch == "." { point = i } else { digits.append(ch) }
    }
    if !text.contains(".") { point = digits.count }
    // the number is 0.<digits> x 10^n
    var n = point + exponent
    while digits.hasPrefix("0") && digits.count > 1 { digits.removeFirst(); n -= 1 }
    while digits.hasSuffix("0") && digits.count > 1 { digits.removeLast() }
    let k = digits.count
    let sign = value < 0 ? "-" : ""
    if k <= n && n <= 21 { return sign + digits + String(repeating: "0", count: n - k) }
    if 0 < n && n <= 21 { return sign + digits.prefix(n) + "." + digits.dropFirst(n) }
    if -6 < n && n <= 0 { return sign + "0." + String(repeating: "0", count: -n) + digits }
    let e = n - 1
    let mantissa = k == 1 ? digits : digits.prefix(1) + "." + digits.dropFirst()
    return sign + mantissa + "e" + (e < 0 ? "-" : "+") + String(abs(e))
}

/// The characters `String.prototype.trim` removes.
private func jsIsSpace(_ scalar: Unicode.Scalar) -> Bool {
    switch scalar.value {
    case 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x20, 0xA0, 0x1680, 0x2000...0x200A, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000, 0xFEFF:
        return true
    default:
        return false
    }
}

/// `text.trim()` in JavaScript.
func jsTrim(_ text: String) -> String {
    let scalars = text.unicodeScalars
    guard let first = scalars.firstIndex(where: { !jsIsSpace($0) }) else { return "" }
    let last = scalars.lastIndex(where: { !jsIsSpace($0) }) ?? first
    return String(scalars[first...last])
}

/// `text.split("\n")`, on code points. Swift reads "\r\n" as one character, so a plain split would miss it.
func jsLines(_ text: String) -> [String] {
    text.unicodeScalars.split(separator: "\n", omittingEmptySubsequences: false).map { String($0) }
}

/// `parseInt(text, 10)`: leading spaces, a sign, then as many digits as there are. Nil is NaN.
func jsParseInt(_ text: String) -> Int? {
    var scalars = Substring.UnicodeScalarView(jsTrim(text).unicodeScalars)
    var negative = false
    if let first = scalars.first, first == "-" || first == "+" {
        negative = first == "-"
        scalars.removeFirst()
    }
    var value = 0
    var seen = false
    for scalar in scalars {
        guard scalar.value >= 48, scalar.value <= 57 else { break }
        seen = true
        let (scaled, over) = value.multipliedReportingOverflow(by: 10)
        let (next, over2) = scaled.addingReportingOverflow(Int(scalar.value - 48))
        if over || over2 { return Int.max }
        value = next
    }
    return seen ? (negative ? -value : value) : nil
}

// MARK: - Reading JSON

/// `JSON.parse`: nil where JavaScript would throw.
func parseCardJSON(_ text: String) -> CardValue? {
    var reader = CardJSONReader(bytes: Array(text.utf8))
    reader.skipSpace()
    guard let value = reader.value(depth: 0) else { return nil }
    reader.skipSpace()
    return reader.atEnd ? value : nil
}

private struct CardJSONReader {
    let bytes: [UInt8]
    var at = 0
    /// Deeper nesting than this is not a card. It also keeps the reader's own stack small.
    static let maxDepth = 200

    init(bytes: [UInt8]) { self.bytes = bytes }

    var atEnd: Bool { at >= bytes.count }
    private var peek: UInt8? { at < bytes.count ? bytes[at] : nil }

    mutating func skipSpace() {
        while let b = peek, b == 0x20 || b == 0x09 || b == 0x0A || b == 0x0D { at += 1 }
    }

    private mutating func take(_ word: String) -> Bool {
        let w = Array(word.utf8)
        guard at + w.count <= bytes.count, Array(bytes[at..<(at + w.count)]) == w else { return false }
        at += w.count
        return true
    }

    mutating func value(depth: Int) -> CardValue? {
        guard depth <= Self.maxDepth, let b = peek else { return nil }
        switch b {
        case UInt8(ascii: "{"): return object(depth: depth)
        case UInt8(ascii: "["): return array(depth: depth)
        case UInt8(ascii: "\""): return string().map(CardValue.string)
        case UInt8(ascii: "t"): return take("true") ? .bool(true) : nil
        case UInt8(ascii: "f"): return take("false") ? .bool(false) : nil
        case UInt8(ascii: "n"): return take("null") ? .null : nil
        default: return number()
        }
    }

    private mutating func object(depth: Int) -> CardValue? {
        at += 1
        var out = CardObject()
        skipSpace()
        if peek == UInt8(ascii: "}") { at += 1; return .object(out) }
        while true {
            skipSpace()
            guard peek == UInt8(ascii: "\""), let key = string() else { return nil }
            skipSpace()
            guard peek == UInt8(ascii: ":") else { return nil }
            at += 1
            skipSpace()
            guard let member = value(depth: depth + 1) else { return nil }
            out[key] = member
            skipSpace()
            if peek == UInt8(ascii: ",") { at += 1; continue }
            if peek == UInt8(ascii: "}") { at += 1; return .object(out) }
            return nil
        }
    }

    private mutating func array(depth: Int) -> CardValue? {
        at += 1
        var out: [CardValue] = []
        skipSpace()
        if peek == UInt8(ascii: "]") { at += 1; return .array(out) }
        while true {
            skipSpace()
            guard let item = value(depth: depth + 1) else { return nil }
            out.append(item)
            skipSpace()
            if peek == UInt8(ascii: ",") { at += 1; continue }
            if peek == UInt8(ascii: "]") { at += 1; return .array(out) }
            return nil
        }
    }

    private mutating func number() -> CardValue? {
        let start = at
        func digit(_ b: UInt8?) -> Bool { b.map { $0 >= 48 && $0 <= 57 } ?? false }
        if peek == UInt8(ascii: "-") { at += 1 }
        guard digit(peek) else { return nil }
        if peek == UInt8(ascii: "0") { at += 1 } else { while digit(peek) { at += 1 } }
        if peek == UInt8(ascii: ".") {
            at += 1
            guard digit(peek) else { return nil }
            while digit(peek) { at += 1 }
        }
        if peek == UInt8(ascii: "e") || peek == UInt8(ascii: "E") {
            at += 1
            if peek == UInt8(ascii: "+") || peek == UInt8(ascii: "-") { at += 1 }
            guard digit(peek) else { return nil }
            while digit(peek) { at += 1 }
        }
        guard let value = Double(String(decoding: bytes[start..<at], as: UTF8.self)) else { return nil }
        return .number(value)
    }

    private mutating func hex4() -> UInt32? {
        guard at + 4 <= bytes.count else { return nil }
        var value: UInt32 = 0
        for b in bytes[at..<(at + 4)] {
            let d: UInt32
            switch b {
            case 48...57: d = UInt32(b) - 48
            case 65...70: d = UInt32(b) - 55
            case 97...102: d = UInt32(b) - 87
            default: return nil
            }
            value = value * 16 + d
        }
        at += 4
        return value
    }

    private mutating func string() -> String? {
        at += 1
        var out: [UInt8] = []
        func append(_ code: UInt32) {
            // A lone surrogate half has no UTF-8 form: it becomes the replacement character.
            let scalar = Unicode.Scalar(code) ?? "\u{FFFD}"
            out.append(contentsOf: Array(String(scalar).utf8))
        }
        while let b = peek {
            at += 1
            switch b {
            case UInt8(ascii: "\""):
                return String(decoding: out, as: UTF8.self)
            case UInt8(ascii: "\\"):
                guard let e = peek else { return nil }
                at += 1
                switch e {
                case UInt8(ascii: "\""): out.append(0x22)
                case UInt8(ascii: "\\"): out.append(0x5C)
                case UInt8(ascii: "/"): out.append(0x2F)
                case UInt8(ascii: "b"): out.append(0x08)
                case UInt8(ascii: "f"): out.append(0x0C)
                case UInt8(ascii: "n"): out.append(0x0A)
                case UInt8(ascii: "r"): out.append(0x0D)
                case UInt8(ascii: "t"): out.append(0x09)
                case UInt8(ascii: "u"):
                    guard let unit = hex4() else { return nil }
                    if unit >= 0xD800 && unit <= 0xDBFF,
                       at + 1 < bytes.count, bytes[at] == UInt8(ascii: "\\"), bytes[at + 1] == UInt8(ascii: "u") {
                        let mark = at
                        at += 2
                        if let low = hex4(), low >= 0xDC00 && low <= 0xDFFF {
                            append(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00))
                        } else {
                            at = mark
                            append(unit)
                        }
                    } else {
                        append(unit)
                    }
                default:
                    return nil
                }
            case 0..<0x20:
                return nil
            default:
                out.append(b)
            }
        }
        return nil
    }
}
