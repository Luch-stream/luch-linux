// Pointer, ink, blank and page messages for a receiver (PROTOCOL.md 4h), as luchd's present.rs makes them safe, and
// the pointer's coalescing to one move a frame.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum Annotate {
    /// The receiver's frame: pointer moves closer together than this are merged into the newest.
    public static let pointerEvery: TimeInterval = 0.016
    static let maxPoints = 2000

    static func unit(_ v: Any?) -> Double? {
        guard let n = v as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID() else { return nil }
        let x = n.doubleValue
        guard x.isFinite else { return nil }
        return (min(max(x, 0), 1) * 10_000).rounded() / 10_000
    }

    static func uint(_ v: Any?) -> Int? {
        guard let n = v as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID() else { return nil }
        let d = n.doubleValue
        return d >= 0 && d == d.rounded() && d < 1e15 ? Int(d) : nil
    }

    static func bool(_ v: Any?) -> Bool? {
        guard let n = v as? NSNumber, CFGetTypeID(n) == CFBooleanGetTypeID() else { return nil }
        return n.boolValue
    }

    static func isColor(_ s: String) -> Bool {
        (s.count == 7 || s.count == 9) && s.hasPrefix("#") && s.dropFirst().allSatisfy(\.isHexDigit)
    }

    /// A pointer, ink, blank or page message made safe to pass on: known fields only, coordinates clamped and
    /// rounded, sizes limited. nil for anything else.
    public static func sanitize(_ v: [String: Any]) -> [String: Any]? {
        switch v["t"] as? String {
        case "pointer":
            if bool(v["show"]) == false { return ["t": "pointer", "show": false] }
            guard let x = unit(v["x"]), let y = unit(v["y"]) else { return nil }
            return ["t": "pointer", "x": x, "y": y]
        case "blank":
            guard let on = bool(v["on"]) else { return nil }
            return ["t": "blank", "on": on]
        case "doc_page":
            guard let p = uint(v["page"]) else { return nil }
            return ["t": "doc_page", "page": p]
        case "ink":
            switch v["do"] as? String {
            case "undo": return ["t": "ink", "do": "undo"]
            case "clear": return ["t": "ink", "do": "clear"]
            case "stroke":
                guard let raw = v["points"] as? [Any], raw.count % 2 == 0, raw.count <= 2 * maxPoints,
                      let id = uint(v["id"]) else { return nil }
                var points: [Double] = []
                for p in raw {
                    guard let u = unit(p) else { return nil }
                    points.append(u)
                }
                var m: [String: Any] = ["t": "ink", "do": "stroke", "id": id, "points": points]
                if let c = v["color"] as? String, isColor(c) { m["color"] = c.lowercased() }
                if let n = v["width"] as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID(), n.doubleValue > 0,
                   n.doubleValue <= 0.1 {
                    m["width"] = (n.doubleValue * 100_000).rounded() / 100_000
                }
                return m
            default:
                return nil
            }
        default:
            return nil
        }
    }
}

/// Pointer moves to the receiver at most every `every` seconds: the first at once, then the newest one when its turn
/// comes; the last one is never lost. Times are monotonic seconds, passed in so tests need no clock.
public struct Coalescer {
    let every: TimeInterval
    private var last: TimeInterval?
    private var pending: [String: Any]?

    public init(every: TimeInterval = Annotate.pointerEvery) {
        self.every = every
    }

    /// A new pointer message: returned when it may go now, else kept (replacing an older one) until `deadline`.
    public mutating func offer(_ now: TimeInterval, _ v: [String: Any]) -> [String: Any]? {
        if pending == nil, last.map({ now >= $0 + every }) ?? true {
            last = now
            return v
        }
        pending = v
        return nil
    }

    public func deadline(now: TimeInterval) -> TimeInterval? {
        guard pending != nil else { return nil }
        return last.map { $0 + every } ?? now
    }

    /// The kept message, once its time has come.
    public mutating func due(_ now: TimeInterval) -> [String: Any]? {
        guard let d = deadline(now: now), now >= d else { return nil }
        last = now
        defer { pending = nil }
        return pending
    }
}
