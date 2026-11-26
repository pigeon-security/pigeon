import Foundation

/// Any JSON value, as the console's read and write paths send and answer it. Read it with the
/// subscripts and accessors (a missing key or index reads as `.null`, so chains never crash), or
/// decode a typed value with [`decode(_:)`]. Literals build it: `["instance": "docs", "limit": 20]`.
enum JSON: Hashable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSON])
    case object([String: JSON])

    /// The value under `key` of an object; `.null` when there is none.
    subscript(key: String) -> JSON {
        if case .object(let o) = self { return o[key] ?? .null }
        return .null
    }

    /// The item at `index` of an array; `.null` when there is none.
    subscript(index: Int) -> JSON {
        if case .array(let a) = self, a.indices.contains(index) { return a[index] }
        return .null
    }

    var string: String? {
        if case .string(let s) = self { return s }
        return nil
    }

    var double: Double? {
        if case .number(let n) = self { return n }
        return nil
    }

    /// The number as an Int, when it is a finite whole number in range.
    var int: Int? {
        guard case .number(let n) = self, n.isFinite, n.rounded() == n,
            n >= Double(Int.min), n <= Double(Int.max)
        else { return nil }
        return Int(n)
    }

    var bool: Bool? {
        if case .bool(let b) = self { return b }
        return nil
    }

    var array: [JSON]? {
        if case .array(let a) = self { return a }
        return nil
    }

    var object: [String: JSON]? {
        if case .object(let o) = self { return o }
        return nil
    }

    var isNull: Bool { self == .null }

    /// The items of an array, or none.
    var items: [JSON] { array ?? [] }

    /// A scalar as text (strings as they are, numbers without a trailing `.0`), or nil.
    var text: String? {
        switch self {
        case .string(let s): return s
        case .number(let n):
            if let i = int { return String(i) }
            return String(n)
        case .bool(let b): return b ? "true" : "false"
        default: return nil
        }
    }

    /// This value decoded as `T`.
    func decode<T: Decodable>(_ type: T.Type = T.self) throws -> T {
        let data = try JSONEncoder().encode(self)
        return try JSONDecoder().decode(T.self, from: data)
    }

    /// Any `Encodable` value as JSON.
    init<T: Encodable>(encoding value: T) throws {
        let data = try JSONEncoder().encode(value)
        self = try JSONDecoder().decode(JSON.self, from: data)
    }

    /// A value from `JSONSerialization` (or the MCP client's answers); anything else is `.null`.
    init(any value: Any?) {
        switch value {
        case nil, is NSNull: self = .null
        case let n as NSNumber:
            // A Bool bridged through Foundation is an NSNumber of type char.
            if CFGetTypeID(n) == CFBooleanGetTypeID() {
                self = .bool(n.boolValue)
            } else {
                self = .number(n.doubleValue)
            }
        case let s as String: self = .string(s)
        case let a as [Any]: self = .array(a.map { JSON(any: $0) })
        case let o as [String: Any]: self = .object(o.mapValues { JSON(any: $0) })
        default: self = .null
        }
    }

    /// Compact JSON text, keys sorted.
    var compactText: String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(self) else { return "null" }
        return String(decoding: data, as: UTF8.self)
    }
}

extension JSON: Codable {
    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .null
        } else if let b = try? c.decode(Bool.self) {
            self = .bool(b)
        } else if let n = try? c.decode(Double.self) {
            self = .number(n)
        } else if let s = try? c.decode(String.self) {
            self = .string(s)
        } else if let a = try? c.decode([JSON].self) {
            self = .array(a)
        } else if let o = try? c.decode([String: JSON].self) {
            self = .object(o)
        } else {
            throw DecodingError.dataCorruptedError(in: c, debugDescription: "not a JSON value")
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let b): try c.encode(b)
        case .number(let n): try c.encode(n)
        case .string(let s): try c.encode(s)
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        }
    }
}

extension JSON: ExpressibleByNilLiteral, ExpressibleByBooleanLiteral, ExpressibleByIntegerLiteral,
    ExpressibleByFloatLiteral, ExpressibleByStringLiteral, ExpressibleByArrayLiteral,
    ExpressibleByDictionaryLiteral
{
    init(nilLiteral: ()) { self = .null }
    init(booleanLiteral value: Bool) { self = .bool(value) }
    init(integerLiteral value: Int) { self = .number(Double(value)) }
    init(floatLiteral value: Double) { self = .number(value) }
    init(stringLiteral value: String) { self = .string(value) }
    init(arrayLiteral elements: JSON...) { self = .array(elements) }
    init(dictionaryLiteral elements: (String, JSON)...) {
        var o: [String: JSON] = [:]
        for (k, v) in elements { o[k] = v }
        self = .object(o)
    }
}
