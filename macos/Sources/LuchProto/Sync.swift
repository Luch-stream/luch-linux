// Sync of several receivers (PROTOCOL.md 4k): the clock packets this sender answers, the playout delay D it chooses
// for a group, and the corrections it keeps per receiver. A port of luch-linux luchd/src/sync.rs; both run the same
// test vectors as the receiver's SenderPolicy, so every sender picks the same D from the same reports.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

extension PacketFlags {
    /// The first packets after the sender's pts base started anew (a group's pipeline).
    public static let timeline = PacketFlags(rawValue: 1 << 3)
    /// A clock packet that answers a request.
    public static let clockReply = PacketFlags(rawValue: 1 << 4)
}

public enum Sync {
    /// The header's kind of a clock packet.
    public static let kindClock: UInt8 = 3
    /// A clock reply: the request's 24-byte header, then t1, t2, t3.
    public static let replySize = Proto.headerSize + 24

    /// A clock request: the receiver's probe number and its send time t1 (its µs); nil for anything else.
    public static func parseRequest(_ b: [UInt8]) -> (seq: UInt32, t1: UInt64)? {
        guard b.count >= Proto.headerSize + 8, b[0] == kindClock, b[1] & PacketFlags.clockReply.rawValue == 0 else {
            return nil
        }
        return (b.be32(8), b.be64(Proto.headerSize))
    }

    /// The reply to a request: its header with the reply flag, then t1 as received, t2 (received) and t3 (sent), both
    /// on the pts clock in µs.
    public static func reply(to req: [UInt8], t2: UInt64, t3: UInt64) -> [UInt8] {
        var r = [UInt8](repeating: 0, count: replySize)
        r.replaceSubrange(0..<Proto.headerSize, with: req[0..<Proto.headerSize])
        r[1] = PacketFlags.clockReply.rawValue
        r.putBE32(16, 24)
        r.replaceSubrange(Proto.headerSize..<Proto.headerSize + 8, with: req[Proto.headerSize..<Proto.headerSize + 8])
        r.putBE64(Proto.headerSize + 8, t2)
        r.putBE64(Proto.headerSize + 16, t3)
        return r
    }

    /// The reply to `pkt` when it is a clock request, t2 and t3 read from `now`; nil for anything else, or before the
    /// pts clock runs.
    public static func answer(_ pkt: [UInt8], now: () -> UInt64?) -> [UInt8]? {
        guard parseRequest(pkt) != nil, let t2 = now() else { return nil }
        return reply(to: pkt, t2: t2, t3: now() ?? t2)
    }

    /// D in the header's unit (0.1 ms), clamped to the field.
    public static func headerUnits(_ ms: Double) -> UInt16 {
        UInt16(min(max((ms * 10).rounded(), 0), Double(UInt16.max)))
    }

    /// Whether a receiver's welcome says it plays at pts + D (older receivers get D = 0 and no group).
    public static func capable(_ welcome: [String: Any]) -> Bool {
        guard let s = welcome["sync"] as? [String: Any], let v = (s["v"] as? NSNumber)?.intValue else { return false }
        return v >= 1 && (s["clock"] as? Bool) == true
    }

    /// The slider's starting point for a route the platform cannot see past: an HDMI ARC soundbar's DSP, a Bluetooth
    /// speaker whose delay the platform does not report (its path then looks as short as a wire's).
    public static func routePresetMs(_ route: String, audioMs: Double?) -> Double {
        switch route {
        case "hdmi_arc": return 40
        case "bluetooth" where (audioMs ?? .infinity) < 100: return 150
        default: return 0
        }
    }

    /// Where a receiver's correction is kept: per receiver and output route (a TV with and without its soundbar).
    public static func offsetKey(_ id: String, route: String) -> String { "\(id)@\(route)" }

    /// A member's part of `start`: the group block.
    public static func groupBlock(id: String, timeline: UInt32, role: GroupRole, mode: SyncMode,
                                  toleranceMs: Double, offsetMs: Double) -> [String: Any] {
        ["id": id, "timeline": timeline, "role": role.rawValue, "mode": mode.wire,
         "tolerance_ms": toleranceMs, "offset_ms": offsetMs]
    }
}

/// What a receiver of a group presents; the sender sends it only those streams.
public enum GroupRole: String, CaseIterable {
    case video, audio, both

    public var video: Bool { self != .audio }
    public var audio: Bool { self != .video }
}

public enum SyncMode: Equatable {
    case interactive
    case watching

    public static func parse(_ s: String) -> SyncMode? {
        switch s {
        case "interactive": return .interactive
        case "watch", "watching": return .watching
        default: return nil
        }
    }

    public var wire: String { self == .interactive ? "interactive" : "watch" }
}

/// How D is chosen for a group. Watching: `headroomMs` above the slowest need, at least the floor, up at once and down
/// in one step after 30 s well below. Interactive: the picture at once, the sound at the later of its own need and the
/// picture; the picture waits only for a speaker beyond the tolerance, by the excess.
public struct SyncPolicy {
    /// Watching: D is set this far above the slowest need, so that its small changes leave D where it is.
    public static let headroomMs = 10.0

    public let mode: SyncMode
    private let toleranceMs: Double
    private let floorMs: Double
    private var dv = 0.0
    private var da = 0.0
    private var lowSince: Double?
    private var first = true

    public init(mode: SyncMode, toleranceMs: Double, floorMs: Double) {
        self.mode = mode
        self.toleranceMs = toleranceMs
        self.floorMs = mode == .watching ? max(floorMs, 0) : 0
    }

    /// One second's reports: each picture receiver's pts → display, each receiver's need (watching: all; interactive:
    /// the sound receivers). Returns (D_v, D_a) in ms.
    public mutating func update(nowS: Double, videoPtsMs: [Double], needMs: [Double]) -> (video: Double, audio: Double) {
        let need = needMs.filter(\.isFinite).max()
        let video = videoPtsMs.filter(\.isFinite).max()
        switch mode {
        case .watching:
            let least = need.map { max(floorMs, $0 + 2) } ?? floorMs
            let set = need.map { max(floorMs, $0 + Self.headroomMs) } ?? floorMs
            let d = step(nowS, d: da, least: least, set: set, rise: 0, waitS: 30, below: 10)
            dv = d
            da = d
        case .interactive:
            let (n, v) = (need ?? 0, video ?? 0)
            let least = max(n, v)
            let d = step(nowS, d: da, least: least, set: least, rise: 1, waitS: 10, below: 5)
            dv = d > v + toleranceMs ? d - toleranceMs : 0
            da = d
        }
        first = false
        func r(_ x: Double) -> Double { (x * 10).rounded() / 10 }
        return (r(dv), r(da))
    }

    /// D moves seldom and in one step, since every change is a resync heard on every device at once: up to `set` as
    /// soon as `least` is beyond it (by more than `rise`); down to `set` once it has been more than `below` under D for
    /// `waitS`.
    private mutating func step(_ now: Double, d: Double, least: Double, set: Double, rise: Double, waitS: Double,
                               below: Double) -> Double {
        if first || least > d + rise || (least > d && d == 0) {
            lowSince = nil
            return set
        }
        if set < d - below {
            let since = lowSince ?? now
            lowSince = since
            if now - since >= waitS {
                lowSince = nil
                return set
            }
        } else {
            lowSince = nil
        }
        return d
    }
}

/// What a receiver of a group reported in its last stats, as the policy needs it.
public struct SyncReport: Equatable {
    public var needMs: Double?
    public var videoPtsMs: Double?
    public var route: String?
    public var audioMs: Double?

    public init(needMs: Double? = nil, videoPtsMs: Double? = nil, route: String? = nil, audioMs: Double? = nil) {
        self.needMs = needMs
        self.videoPtsMs = videoPtsMs
        self.route = route
        self.audioMs = audioMs
    }

    public init(stats s: [String: Any]) {
        let out = s["out"] as? [String: Any] ?? [:]
        self.init(needMs: (s["need_ms"] as? NSNumber)?.doubleValue,
                  videoPtsMs: (out["video_pts_ms"] as? NSNumber)?.doubleValue,
                  route: out["route"] as? String,
                  audioMs: (out["audio_ms"] as? NSNumber)?.doubleValue)
    }
}

extension Array where Element == UInt8 {
    mutating func putBE64(_ at: Int, _ v: UInt64) {
        for i in 0..<8 { self[at + i] = UInt8(truncatingIfNeeded: v >> (56 - 8 * UInt64(i))) }
    }

    func be64(_ at: Int) -> UInt64 {
        (0..<8).reduce(UInt64(0)) { $0 << 8 | UInt64(self[at + $1]) }
    }
}
