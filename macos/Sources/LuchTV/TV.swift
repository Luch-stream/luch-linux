// TVs without the Luch app (luch-linux docs/other-tvs.md): DLNA/UPnP renderers and Chromecasts play what `cast` is
// given, fetching it over HTTP from the FileServer like a Luch receiver does. They are found next to the Luch
// receivers (SSDP, mDNS `_googlecast._tcp`), shown in the same list, one row per TV, and driven by the same local
// commands: cast, media_cmd, media_state, stop_cast, volume. What they report is told as Luch's `media_state`.
// The same design as luchd/src/tv/mod.rs.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation

/// How a TV is talked to, best first: Luch (its own receiver: mirroring, the lowest latency, everything), then Google
/// Cast (state pushed to us, subtitles, speed, about 2 s to start), then DLNA (state polled once a second, subtitles
/// only on some TVs, one speed).
public enum TVKind: String, CaseIterable, Comparable {
    case luch, cast, dlna

    /// From a row's id: TVs without Luch have "cast:" or "dlna:" ids, as luchctl's kind_of on Linux.
    public static func of(_ id: String) -> TVKind {
        if id.hasPrefix("cast:") { return .cast }
        if id.hasPrefix("dlna:") { return .dlna }
        return .luch
    }

    var rank: Int { TVKind.allCases.firstIndex(of: self)! }
    public static func < (a: TVKind, b: TVKind) -> Bool { a.rank < b.rank }

    /// What can be done over it.
    public var can: [String] {
        switch self {
        case .luch: return ["mirror", "media", "photo", "doc", "apps", "volume", "subs", "rate"]
        case .cast: return ["media", "photo", "volume", "subs", "rate"]
        case .dlna: return ["media", "photo", "volume", "subs"]
        }
    }

    /// About how long a press takes to show on the TV (ms), measured on real devices of each kind.
    public var controlMs: Int {
        switch self {
        case .luch: return 30
        case .cast: return 150
        case .dlna: return 600
        }
    }
}

public struct TVError: Error, LocalizedError, Equatable {
    public let message: String
    public init(_ message: String) { self.message = message }
    public var errorDescription: String? { message }
}

/// A TV found on the network that is not (only) a Luch receiver.
public struct Device: Equatable {
    /// "dlna:<UDN>" or "cast:<id>": stable across restarts, never a Luch id.
    public var id: String
    public var kind: TVKind
    public var name: String
    public var model: String
    public var address: String
    public var port: UInt16
    public var control: Control

    public enum Control: Equatable {
        /// AVTransport's control URL and service type as announced (SOAP actions are named with it), and
        /// RenderingControl's when there is one.
        case dlna(avTransport: String, avType: String, rendering: (url: String, type: String)?)
        case cast

        public static func == (a: Control, b: Control) -> Bool {
            switch (a, b) {
            case (.cast, .cast): return true
            case let (.dlna(u1, t1, r1), .dlna(u2, t2, r2)):
                return u1 == u2 && t1 == t2 && r1?.url == r2?.url && r1?.type == r2?.type
            default: return false
            }
        }
    }

    public init(id: String, kind: TVKind, name: String, model: String, address: String, port: UInt16, control: Control) {
        self.id = id
        self.kind = kind
        self.name = name
        self.model = model
        self.address = address
        self.port = port
        self.control = control
    }
}

// MARK: - one list

/// One way to reach a TV, as the list is built from: Luch receivers and devices alike.
public struct Entry: Equatable {
    public var kind: TVKind
    public var id: String
    public var name: String
    public var model: String
    public var address: String
    public var port: UInt16

    public init(kind: TVKind, id: String, name: String, model: String, address: String, port: UInt16) {
        self.kind = kind
        self.id = id
        self.name = name
        self.model = model
        self.address = address
        self.port = port
    }

    public init(_ d: Device) {
        self.init(kind: d.kind, id: d.id, name: d.name, model: d.model, address: d.address, port: d.port)
    }
}

/// Puts the ways that lead to one TV together: the same address, each kind once (two Chromecasts at one address are
/// a TV and a speaker group, two rows). Every group's ways go best first; groups come in a stable order, by name.
public func group(_ entries: [Entry]) -> [[Entry]] {
    let all = entries.sorted { ($0.kind, $0.id) < ($1.kind, $1.id) }
    var groups: [[Entry]] = []
    for e in all {
        if let i = groups.firstIndex(where: { $0[0].address == e.address && $0.allSatisfy { $0.kind != e.kind } }) {
            groups[i].append(e)
        } else {
            groups.append([e])
        }
    }
    return groups.sorted { ($0[0].name, $0[0].id) < ($1[0].name, $1[0].id) }
}

/// The way to use for a row id: the best, or the one of kind `forced`. Nil when the id names no row, or the TV has
/// no such way.
public func pick(_ groups: [[Entry]], id: String, forced: TVKind?) -> Entry? {
    guard let g = groups.first(where: { $0.contains { $0.id == id } }) else { return nil }
    guard let forced else { return g.first }
    return g.first { $0.kind == forced }
}

// MARK: - what to play

public struct Sub: Equatable {
    public var url: String
    public var mime: String
    public var lang: String

    public init(url: String, mime: String, lang: String) {
        self.url = url
        self.mime = mime
        self.lang = lang
    }
}

/// One thing for a TV to play or show, from the message `Cast.build` makes for a Luch receiver.
public struct Media: Equatable {
    public var url = ""
    public var mime = ""
    public var title = ""
    public var start = 0.0
    public var paused = false
    public var poster: String?
    public var subs: [Sub] = []
    /// A picture rather than something that plays.
    public var photo = false
    /// Served by this Mac (a local file or relayed), so byte ranges work; otherwise the TV fetches a site itself.
    public var ours = false

    public init() {}

    /// The first (best) source of a `media` message, or the first picture of a `photo` one. Documents and the TV's
    /// apps are Luch's alone.
    public init(message m: [String: Any]) throws {
        let s = { (v: Any?) in v as? String ?? "" }
        switch m["t"] as? String {
        case "media":
            let src = (m["src"] as? [[String: Any]])?.first ?? [:]
            url = s(src["url"])
            mime = s(src["mime"])
            title = s(m["title"])
            start = (m["start"] as? NSNumber)?.doubleValue ?? 0
            paused = m["paused"] as? Bool ?? false
            poster = m["poster"] as? String
            subs = (m["subs"] as? [[String: Any]] ?? []).map { Sub(url: s($0["url"]), mime: s($0["mime"]), lang: s($0["lang"])) }
            ours = m["via"] as? String == "sender"
        case "photo":
            let p = (m["items"] as? [[String: Any]])?.first ?? [:]
            url = s(p["url"])
            mime = s(p["mime"])
            title = s(p["name"])
            photo = true
            ours = true
        case "doc": throw TVError("this TV cannot show documents: only a Luch receiver can")
        case "open_app": throw TVError("this TV cannot open YouTube from here: only a Luch receiver can")
        default: throw TVError("nothing this TV can play")
        }
    }

    /// The MIME type to announce: the known one, else what most TVs take.
    var mimeOrDefault: String {
        mime.isEmpty ? (photo ? "image/jpeg" : "video/mp4") : mime
    }
}

// MARK: - a session

/// What a session is asked: a `media_cmd` (without "t"), a Luch `volume` message, to end (the TV stops), or to leave
/// (something newer plays there now: the TV is left alone).
public enum Ask {
    case cmd([String: Any])
    case volume([String: Any])
    case stop
    case leave
}

/// A monotonic clock in seconds.
func now() -> Double { ProcessInfo.processInfo.systemUptime }

/// The answer to one ask, waited for by whoever asked.
final class Reply {
    private let done = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var result: Result<[String: Any], TVError>?

    func send(_ r: Result<[String: Any], TVError>) {
        lock.lock()
        let first = result == nil
        if first { result = r }
        lock.unlock()
        if first { done.signal() }
    }

    func wait(_ seconds: Double) -> Result<[String: Any], TVError>? {
        guard done.wait(timeout: .now() + seconds) == .success else { return nil }
        lock.lock()
        defer { lock.unlock() }
        return result
    }
}

/// A queue one thread waits on, with a deadline.
final class Mailbox<T> {
    private let cond = NSCondition()
    private var items: [T] = []
    private(set) var closed = false

    /// False when closed: nobody takes it any more.
    @discardableResult
    func put(_ x: T) -> Bool {
        cond.lock()
        defer { cond.unlock() }
        guard !closed else { return false }
        items.append(x)
        cond.signal()
        return true
    }

    /// The next item, or nil once `until` (a `now()` time) has passed.
    func take(until: Double) -> T? {
        cond.lock()
        defer { cond.unlock() }
        while items.isEmpty {
            let left = until - now()
            if left <= 0 { return nil }
            _ = cond.wait(until: Date().addingTimeInterval(left))
        }
        return items.removeFirst()
    }

    /// Nothing more goes in; what is left is handed back.
    func close() -> [T] {
        cond.lock()
        closed = true
        let left = items
        items = []
        cond.unlock()
        return left
    }
}

/// A TV playing something for us. Its thread tells `media_state`s and ends with a `stopped` one.
public final class TVSession {
    let asks = Mailbox<(Ask, Reply)>()
    let tell = Teller()
    public let device: Device

    init(device: Device, onState: StateSink?) {
        self.device = device
        tell.onState = { [weak self] v in
            if let self { onState?(self, v) }
        }
    }

    /// Asks the session; waits up to 10 s for the TV.
    public func ask(_ a: Ask) -> Result<[String: Any], TVError> {
        let r = Reply()
        guard asks.put((a, r)) else { return .failure(TVError("the TV is no longer playing")) }
        return r.wait(10) ?? .failure(TVError("the TV did not answer"))
    }

    public var state: [String: Any] { tell.last }
    public var ended: Bool { asks.closed }

    private let volumeLock = NSLock()
    private var volumeNow: [String: Any]?

    /// The TV's volume as it last said ({"level":N,"muted":B}); nil until it has.
    public var volume: [String: Any]? {
        volumeLock.lock()
        defer { volumeLock.unlock() }
        return volumeNow
    }

    func noteVolume(_ v: [String: Any]) {
        volumeLock.lock()
        volumeNow = v
        volumeLock.unlock()
    }

    /// The session's thread is done: asks still waiting are told so.
    func finish(_ why: String) {
        for (_, r) in asks.close() { r.send(.failure(TVError("the TV is no longer playing"))) }
        tell.stopped(why)
    }
}

/// Tells states as Luch receivers do (PROTOCOL.md 4b): on every change of state or rate, on a jump of the position
/// (a seek), and every 5 s while playing; `by` says whether our ask or the TV's own remote caused it.
final class Teller {
    private let lock = NSLock()
    private var lastValue: [String: Any] = [:]
    private var lastSent: Double?
    private var askedAt: Double?
    private var force = false
    var onState: (([String: Any]) -> Void)?

    var last: [String: Any] {
        lock.lock()
        defer { lock.unlock() }
        return lastValue
    }

    /// We just asked the TV something: the next state is told even if it looks the same (a short seek, "do":
    /// "state"), and what follows is "by" the sender.
    func asked() {
        lock.lock()
        askedAt = now()
        force = true
        lock.unlock()
    }

    /// The TV's state now: "loading", "playing", "paused", "ended" or "error", where it is and how long it is.
    func tell(_ state: String, pos: Double, dur: Double?, rate: Double) {
        let pos = (pos * 10).rounded() / 10
        lock.lock()
        let since = lastSent.map { now() - $0 }
        let was = lastValue["state"] as? String ?? ""
        let lastPos = (lastValue["pos"] as? Double) ?? 0
        let lastRate = lastValue["rate"] as? Double
        let expected = was == "playing" && since != nil ? lastPos + since! * (lastRate ?? 1) : lastPos
        let changed = was != state || lastRate != rate || abs(pos - expected) > 2.5 || (lastValue["dur"] as? Double) != dur
        let due = force || (state == "playing" && (since ?? 5) >= 5)
        let by = askedAt.map { now() - $0 < 3 } == true ? "sender" : "receiver"
        lock.unlock()
        guard changed || due else { return }
        var v: [String: Any] = ["t": "media_state", "state": state, "pos": pos, "rate": rate, "by": by,
                                "live": dur == nil && state != "loading" && state != "error"]
        if let dur { v["dur"] = dur }
        send(v)
    }

    /// The end: `reason` "ended" (played to the end), "sender" (stop_cast), "receiver" (stopped on the TV), "replaced"
    /// (something else plays there now) or "gone" (the TV went away).
    func stopped(_ reason: String) {
        send(["t": "media_state", "state": "stopped", "reason": reason])
    }

    func error(_ detail: String) {
        send(["t": "media_state", "state": "error", "detail": detail])
    }

    private func send(_ v: [String: Any]) {
        lock.lock()
        force = false
        lastValue = v
        lastSent = now()
        let f = onState
        lock.unlock()
        f?(v)
    }
}

/// Every media_state of a session, the last one `stopped`; called on the session's thread, from the first one on.
public typealias StateSink = (TVSession, [String: Any]) -> Void

/// Starts playing `m` on `d`; returns once the TV has taken it.
public func play(_ d: Device, _ m: Media, onState: StateSink? = nil) throws -> TVSession {
    switch d.control {
    case .dlna: return try DLNA.play(d, m, onState: onState)
    case .cast: return try CastV2.play(d, m, onState: onState)
    }
}

/// The address of this machine the TV reaches: the source the routing table picks for it (no packet leaves).
public func localAddress(for tv: String) -> String? {
    let fd = socket(AF_INET, SOCK_DGRAM, 0)
    guard fd >= 0 else { return nil }
    defer { close(fd) }
    var a = sockaddr_in()
    a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
    a.sin_family = sa_family_t(AF_INET)
    a.sin_port = UInt16(9).bigEndian
    guard inet_pton(AF_INET, tv, &a.sin_addr) == 1 else { return nil }
    let ok = withUnsafePointer(to: &a) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
    } == 0
    guard ok else { return nil }
    var me = sockaddr_in()
    var len = socklen_t(MemoryLayout<sockaddr_in>.size)
    withUnsafeMutablePointer(to: &me) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { _ = getsockname(fd, $0, &len) } }
    var buf = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
    inet_ntop(AF_INET, &me.sin_addr, &buf, socklen_t(INET_ADDRSTRLEN))
    return String(cString: buf)
}

/// "H:MM:SS", the time format of AVTransport.
public func hms(_ t: Double) -> String {
    let t = Int(max(0, t))
    return String(format: "%d:%02d:%02d", t / 3600, t / 60 % 60, t % 60)
}

/// "H+:MM:SS[.F+]" (also "MM:SS", fractions as decimals or "F0/F1") in seconds; nil for "NOT_IMPLEMENTED" or "".
public func parseHMS(_ s: String) -> Double? {
    let s = s.trimmingCharacters(in: .whitespaces)
    let parts = s.split(separator: ".", maxSplits: 1, omittingEmptySubsequences: false)
    let whole = parts[0], frac = parts.count > 1 ? parts[1] : ""
    var t = 0.0
    for p in whole.split(separator: ":", omittingEmptySubsequences: false) {
        guard let n = UInt64(p) else { return nil }
        t = t * 60 + Double(n)
    }
    var f = 0.0
    if let slash = frac.firstIndex(of: "/") {
        guard let a = Double(frac[..<slash]), let b = Double(frac[frac.index(after: slash)...]), b > 0 else { return nil }
        f = a / b
    } else if !frac.isEmpty {
        guard frac.allSatisfy(\.isNumber), let x = Double("0.\(frac)") else { return nil }
        f = x
    }
    return t + f
}

/// A JSON object as text, for the wire.
func json(_ v: [String: Any]) -> String {
    let d = (try? JSONSerialization.data(withJSONObject: v, options: [.sortedKeys, .withoutEscapingSlashes])) ?? Data("{}".utf8)
    return String(decoding: d, as: UTF8.self)
}
