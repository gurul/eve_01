// A json-render block's text to one loose JSON document: SpecStream lines (one RFC 6902 patch per line) applied in
// order, or one whole JSON spec. A port of compileSpec in the web page's specProgram.ts and of the patch functions
// it calls in @json-render/core, including what those do with a path they cannot follow.
// Behaviour: docs/app-chat.md, "The card engine".
import Foundation

/// A patch JavaScript would throw on. The line counts as bad.
private struct PatchFailure: Error {}

/// The block's lines applied in order. `bad` counts lines that are not a patch, or that could not be applied.
func compileCard(_ code: String, streaming: Bool) -> (spec: CardValue, bad: Int) {
    let whole = jsTrim(code)
    if whole.hasPrefix("{"), case .object(let parsed)? = parseCardJSON(whole),
       parsed["elements"]?.objectValue != nil, !parsed.has("op") {
        return (.object(parsed), 0)
    }
    let lines = jsLines(code).map(jsTrim).filter { !$0.isEmpty }
    var spec = CardValue.object(CardObject())
    var bad = 0
    for (i, line) in lines.enumerated() {
        guard let patch = parsePatchLine(line) else {
            // the last line of a streaming block is still being written
            if !(streaming && i == lines.count - 1) { bad += 1 }
            continue
        }
        do { try applyPatch(&spec, patch) } catch { bad += 1 }
    }
    return (spec, bad)
}

/// One line as a patch: a JSON object with an `op` and a `path`. Nil for anything else.
private func parsePatchLine(_ line: String) -> CardObject? {
    guard line.hasPrefix("{"), case .object(let patch)? = parseCardJSON(line) else { return nil }
    guard patch["op"]?.isTruthy == true, patch.has("path") else { return nil }
    return patch
}

private func applyPatch(_ root: inout CardValue, _ patch: CardObject) throws {
    switch patch["op"]?.stringValue {
    case "add":
        try write(&root, try segments(patch["path"]), patch["value"] ?? .null, insert: true)
    case "replace":
        try write(&root, try segments(patch["path"]), patch["value"] ?? .null, insert: false)
    case "remove":
        try remove(&root, try segments(patch["path"]))
    case "move":
        guard let from = patch["from"], from.isTruthy else { return }
        let moved = try read(root, from)
        try remove(&root, try segments(from))
        try write(&root, try segments(patch["path"]), moved ?? .null, insert: true)
    case "copy":
        guard let from = patch["from"], from.isTruthy else { return }
        let copied = try read(root, from)
        try write(&root, try segments(patch["path"]), copied ?? .null, insert: true)
    case "test":
        guard try read(root, patch["path"]) == patch["value"] else { throw PatchFailure() }
    default:
        return
    }
}

// MARK: - JSON Pointer

/// A pointer's parts. Only a string is a pointer.
private func segments(_ path: CardValue?) throws -> [String] {
    guard case .string(let text)? = path else { throw PatchFailure() }
    let raw = text.hasPrefix("/") ? String(text.dropFirst()) : text
    return raw.split(separator: "/", omittingEmptySubsequences: false).map {
        $0.replacingOccurrences(of: "~1", with: "/").replacingOccurrences(of: "~0", with: "~")
    }
}

private func isDigits(_ text: String) -> Bool {
    !text.isEmpty && text.utf8.allSatisfy { $0 >= 48 && $0 <= 57 }
}

/// An index a write may use: digits, and no further than one past the end.
private func denseIndex(_ count: Int, _ segment: String) -> Int? {
    guard isDigits(segment), let index = Int(segment), index <= count else { return nil }
    return index
}

/// What JavaScript's `typeof` calls an object: an object, an array, or null.
private func isObjectLike(_ value: CardValue?) -> Bool {
    switch value {
    case .object?, .array?, .null?: return true
    default: return false
    }
}

/// `addByPath` (insert) and `setByPath`: containers on the way are made as needed.
private func write(_ root: inout CardValue, _ path: [String], _ value: CardValue, insert: Bool) throws {
    guard !path.isEmpty, !path.contains("__proto__") else { return }
    try write(&root, path[...], value, insert: insert)
}

private func write(_ current: inout CardValue, _ path: ArraySlice<String>, _ value: CardValue, insert: Bool) throws {
    guard let segment = path.first else { return }
    if path.count == 1 {
        switch current {
        case .array(var array):
            if segment == "-" {
                array.append(value)
            } else if let index = denseIndex(array.count, segment) {
                if insert || index == array.count { array.insert(value, at: index) } else { array[index] = value }
            }
            current = .array(array)
        case .object(var object):
            object[segment] = value
            current = .object(object)
        default:
            throw PatchFailure()
        }
        return
    }
    let next = path[path.index(after: path.startIndex)]
    let fresh: CardValue = isDigits(next) || next == "-" ? .array([]) : .object(CardObject())
    switch current {
    case .array(var array):
        guard let index = denseIndex(array.count, segment) else { return }
        if index == array.count { array.append(fresh) } else if !isObjectLike(array[index]) { array[index] = fresh }
        var child = array[index]
        defer { array[index] = child; current = .array(array) }
        try write(&child, path.dropFirst(), value, insert: insert)
    case .object(var object):
        var child = isObjectLike(object[segment]) ? object[segment]! : fresh
        defer { object[segment] = child; current = .object(object) }
        try write(&child, path.dropFirst(), value, insert: insert)
    default:
        throw PatchFailure()
    }
}

/// `removeByPath`: nothing happens when the path leads nowhere.
private func remove(_ root: inout CardValue, _ path: [String]) throws {
    guard !path.isEmpty, !path.contains("__proto__") else { return }
    try remove(&root, path[...])
}

private func remove(_ current: inout CardValue, _ path: ArraySlice<String>) throws {
    guard let segment = path.first else { return }
    switch current {
    case .array(var array):
        guard let index = jsParseInt(segment), array.indices.contains(index) else { return }
        if path.count == 1 {
            array.remove(at: index)
            current = .array(array)
            return
        }
        guard isObjectLike(array[index]) else { return }
        var child = array[index]
        defer { array[index] = child; current = .array(array) }
        try remove(&child, path.dropFirst())
    case .object(var object):
        if path.count == 1 {
            object[segment] = nil
            current = .object(object)
            return
        }
        guard var child = object[segment], isObjectLike(child) else { return }
        defer { object[segment] = child; current = .object(object) }
        try remove(&child, path.dropFirst())
    default:
        throw PatchFailure()
    }
}

/// `getByPath`: nil is JavaScript's undefined.
private func read(_ root: CardValue, _ path: CardValue?) throws -> CardValue? {
    guard let path, path.isTruthy else { return root }
    if path == .string("/") { return root }
    var current: CardValue? = root
    for segment in try segments(path) {
        switch current {
        case .array(let array)?:
            guard let index = jsParseInt(segment), array.indices.contains(index) else { return nil }
            current = array[index]
        case .object(let object)?:
            guard let member = object[segment] else { return nil }
            current = member
        default:
            return nil
        }
    }
    return current
}
