// Input datagrams from the receiver (PROTOCOL.md 4i): a 20-byte header, then type-length records. The records carry
// state, not events, so one lost datagram costs only an in-between position. The same format as luchd's wire.rs.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum InputWire {
    public static let kind: UInt8 = 3
    public static let version: UInt8 = 0
    public static let header = 20

    public static let recTouch: UInt8 = 1
    public static let recPointer: UInt8 = 2
    public static let recKeys: UInt8 = 3
    public static let recGamepad: UInt8 = 4

    public enum Tool: UInt8 {
        case finger = 1, stylus = 2, eraser = 3
    }

    /// One contact that is down (or a stylus hovering) now.
    public struct Contact: Equatable {
        public var id: UInt8
        public var tool: Tool
        /// The stylus's barrel button.
        public var button = false
        /// Near the screen, not touching it (a stylus only).
        public var hover = false
        public var x: UInt16
        public var y: UInt16
        /// 0..65535, 0 when the receiver does not know.
        public var pressure: UInt16 = 0

        public init(id: UInt8, tool: Tool, button: Bool = false, hover: Bool = false, x: UInt16, y: UInt16,
                    pressure: UInt16 = 0) {
            self.id = id
            self.tool = tool
            self.button = button
            self.hover = hover
            self.x = x
            self.y = y
            self.pressure = pressure
        }
    }

    public struct Pointer: Equatable {
        /// bit 0 left, 1 right, 2 middle, 3 back, 4 forward.
        public var buttons: UInt8 = 0
        public var abs: (x: UInt16, y: UInt16)?
        /// The wheel comes from a touchpad: continuous, not in notches.
        public var smooth = false
        /// Running totals since the session began, 1/16 logical pixel.
        public var rel: (x: Int32, y: Int32) = (0, 0)
        /// Running totals, 120 per notch, positive down / right.
        public var wheel: (y: Int32, x: Int32) = (0, 0)

        public init(buttons: UInt8 = 0, abs: (x: UInt16, y: UInt16)? = nil, smooth: Bool = false,
                    rel: (x: Int32, y: Int32) = (0, 0), wheel: (y: Int32, x: Int32) = (0, 0)) {
            self.buttons = buttons
            self.abs = abs
            self.smooth = smooth
            self.rel = rel
            self.wheel = wheel
        }

        public static func == (a: Pointer, b: Pointer) -> Bool {
            a.buttons == b.buttons && a.abs?.x == b.abs?.x && a.abs?.y == b.abs?.y && a.smooth == b.smooth
                && a.rel == b.rel && a.wheel == b.wheel
        }
    }

    public struct Keys: Equatable {
        /// Number of the first event in `events`; the next ones count up from it.
        public var first: UInt16
        public var events: [(code: UInt16, down: Bool)]
        public var held: [UInt16]

        public init(first: UInt16, events: [(code: UInt16, down: Bool)], held: [UInt16]) {
            self.first = first
            self.events = events
            self.held = held
        }

        public static func == (a: Keys, b: Keys) -> Bool {
            a.first == b.first && a.held == b.held && a.events.count == b.events.count
                && zip(a.events, b.events).allSatisfy { $0.code == $1.code && $0.down == $1.down }
        }
    }

    public struct Pad: Equatable {
        public var index: UInt8
        public var connected: Bool
        /// SDL order: A, B, X, Y, back, guide, start, left stick, right stick, LB, RB, up, down, left, right.
        public var buttons: UInt32 = 0
        public var left: (x: Int16, y: Int16) = (0, 0)
        public var right: (x: Int16, y: Int16) = (0, 0)
        public var triggers: (left: UInt16, right: UInt16) = (0, 0)

        public init(index: UInt8, connected: Bool, buttons: UInt32 = 0, left: (x: Int16, y: Int16) = (0, 0),
                    right: (x: Int16, y: Int16) = (0, 0), triggers: (left: UInt16, right: UInt16) = (0, 0)) {
            self.index = index
            self.connected = connected
            self.buttons = buttons
            self.left = left
            self.right = right
            self.triggers = triggers
        }

        public static func == (a: Pad, b: Pad) -> Bool {
            a.index == b.index && a.connected == b.connected && a.buttons == b.buttons && a.left == b.left
                && a.right == b.right && a.triggers == b.triggers
        }
    }

    public enum Record: Equatable {
        case touch([Contact])
        case pointer(Pointer)
        case keys(Keys)
        case gamepad(Pad)
    }

    public struct Packet: Equatable {
        public var seq: UInt32
        public var timeUS: UInt32
        public var token: UInt64
        public var records: [Record]

        public init(seq: UInt32, timeUS: UInt32 = 0, token: UInt64, records: [Record]) {
            self.seq = seq
            self.timeUS = timeUS
            self.token = token
            self.records = records
        }
    }

    public enum ParseError: Error, Equatable {
        case short, notInput, unknownVersion, badRecord, recordCutShort
    }

    /// `a` comes after `b` in a wrapping u32 sequence.
    public static func newer(_ a: UInt32, _ b: UInt32) -> Bool {
        a != b && a &- b < 1 << 31
    }

    /// `a` comes after `b` in a wrapping u16 event numbering.
    public static func newer16(_ a: UInt16, _ b: UInt16) -> Bool {
        a != b && a &- b < 1 << 15
    }

    /// The token where a datagram carries it, without parsing the rest.
    public static func token(of buf: [UInt8]) -> UInt64? {
        guard buf.count >= header else { return nil }
        var r = Reader(buf[12..<20])
        return r.u64()
    }

    /// Parses one datagram. A record cut short or malformed spoils the whole datagram (it is garbage, not input);
    /// an unknown record type is skipped.
    public static func parse(_ buf: [UInt8]) throws -> Packet {
        guard buf.count >= header else { throw ParseError.short }
        guard buf[0] == kind else { throw ParseError.notInput }
        guard buf[1] == version else { throw ParseError.unknownVersion }
        var h = Reader(buf[4..<header])
        let seq = h.u32()!, time = h.u32()!, token = h.u64()!
        var rest = Reader(buf[header...])
        var records: [Record] = []
        while !rest.isEmpty {
            guard let type = rest.u8(), let len = rest.u8() else { throw ParseError.badRecord }
            guard let body = rest.take(Int(len)) else { throw ParseError.recordCutShort }
            var r = Reader(body)
            let record: Record?
            switch type {
            case recTouch: record = touch(&r)
            case recPointer: record = pointer(&r)
            case recKeys: record = keys(&r)
            case recGamepad: record = gamepad(&r)
            default: continue
            }
            guard let record else { throw ParseError.badRecord }
            records.append(record)
        }
        return Packet(seq: seq, timeUS: time, token: token, records: records)
    }

    private static func touch(_ r: inout Reader) -> Record? {
        guard let n = r.u8() else { return nil }
        var v: [Contact] = []
        for _ in 0..<n {
            guard let id = r.u8(), let t = r.u8(), let tool = Tool(rawValue: t & 0x3f),
                  let x = r.u16(), let y = r.u16(), let p = r.u16() else { return nil }
            v.append(Contact(id: id, tool: tool, button: t & 0x40 != 0, hover: t & 0x80 != 0 && tool != .finger,
                             x: x, y: y, pressure: p))
        }
        return .touch(v)
    }

    private static func pointer(_ r: inout Reader) -> Record? {
        guard let buttons = r.u8(), let flags = r.u8(), let x = r.u16(), let y = r.u16(),
              let rx = r.i32(), let ry = r.i32(), let wy = r.i32(), let wx = r.i32() else { return nil }
        return .pointer(Pointer(buttons: buttons, abs: flags & 1 != 0 ? (x, y) : nil, smooth: flags & 2 != 0,
                                rel: (rx, ry), wheel: (wy, wx)))
    }

    private static func keys(_ r: inout Reader) -> Record? {
        guard let first = r.u16(), let n = r.u8() else { return nil }
        var events: [(code: UInt16, down: Bool)] = []
        for _ in 0..<n {
            guard let code = r.u16(), let down = r.u8() else { return nil }
            events.append((code, down != 0))
        }
        guard let h = r.u8() else { return nil }
        var held: [UInt16] = []
        for _ in 0..<h {
            guard let code = r.u16() else { return nil }
            held.append(code)
        }
        return .keys(Keys(first: first, events: events, held: held))
    }

    private static func gamepad(_ r: inout Reader) -> Record? {
        guard let index = r.u8(), let flags = r.u8(), let buttons = r.u32(),
              let lx = r.i16(), let ly = r.i16(), let rx = r.i16(), let ry = r.i16(),
              let lt = r.u16(), let rt = r.u16() else { return nil }
        return .gamepad(Pad(index: index, connected: flags & 1 != 0, buttons: buttons, left: (lx, ly),
                            right: (rx, ry), triggers: (lt, rt)))
    }

    /// Big-endian reads off the front of a slice.
    struct Reader {
        private var rest: ArraySlice<UInt8>
        init(_ s: ArraySlice<UInt8>) { rest = s }
        var isEmpty: Bool { rest.isEmpty }

        mutating func take(_ n: Int) -> ArraySlice<UInt8>? {
            guard rest.count >= n else { return nil }
            defer { rest = rest.dropFirst(n) }
            return rest.prefix(n)
        }

        mutating func u8() -> UInt8? { take(1)?.first }
        mutating func u16() -> UInt16? { take(2).map { $0.reduce(0) { $0 << 8 | UInt16($1) } } }
        mutating func u32() -> UInt32? { take(4).map { $0.reduce(0) { $0 << 8 | UInt32($1) } } }
        mutating func u64() -> UInt64? { take(8).map { $0.reduce(0) { $0 << 8 | UInt64($1) } } }
        mutating func i16() -> Int16? { u16().map { Int16(bitPattern: $0) } }
        mutating func i32() -> Int32? { u32().map { Int32(bitPattern: $0) } }
    }
}
