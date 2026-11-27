// Turns the receiver's successive states into input events: what went down, what moved, what was let go. A port of
// luchd's input/state.rs, so both senders read the same datagrams the same way.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum InputEvent: Equatable {
    /// Positions are normalized to the picture, 0..1.
    case touchDown(slot: UInt32, x: Double, y: Double)
    case touchMotion(slot: UInt32, x: Double, y: Double)
    case touchUp(slot: UInt32)
    /// The stylus is near the screen or on it.
    case pen(Pen)
    /// The stylus went away.
    case penOut
    case pointerAbs(x: Double, y: Double)
    /// Logical pixels.
    case pointerRel(dx: Double, dy: Double)
    /// Linux evdev BTN_* code.
    case button(code: UInt16, down: Bool)
    /// Touchpad scrolling in logical pixels; `finish` ends a run.
    case scroll(dx: Double, dy: Double, finish: Bool)
    /// Mouse wheel notches, positive down / right.
    case notches(vertical: Int32, horizontal: Int32)
    /// Linux evdev KEY_* code.
    case key(code: UInt16, down: Bool)
    case pad(InputWire.Pad)
    case padGone(UInt8)

    public struct Pen: Equatable {
        public var x: Double
        public var y: Double
        /// 0..1; a touch with unknown pressure counts as half.
        public var pressure: Double
        public var touching: Bool
        public var eraser: Bool
        public var button: Bool
    }
}

public struct InputTracker {
    /// No datagram for this long while something is held: everything is let go (PROTOCOL.md 4i).
    public static let watchdog: TimeInterval = 0.5
    /// Logical pixels one wheel notch scrolls on a smooth (touchpad) wheel: libinput's figure for a notch.
    public static let pxPerNotch = 15.0
    public static let btnLeft: UInt16 = 0x110
    /// Pointer buttons in the order of the record's bits: left, right, middle, back (side), forward (extra).
    public static let buttons: [UInt16] = [btnLeft, 0x111, 0x112, 0x116, 0x115]

    public let token: UInt64
    private var lastSeq: UInt32?
    private var lastRx: TimeInterval?
    private var fingers: [UInt8: (UInt16, UInt16)] = [:]
    private var pen: InputEvent.Pen?
    private var pointer: InputWire.Pointer?
    private var wheelRest: (y: Int32, x: Int32) = (0, 0)
    private var scrolling = false
    private var keysApplied: UInt16?
    private var held = Set<UInt16>()
    private var pads: [UInt8: InputWire.Pad] = [:]

    public init(token: UInt64) { self.token = token }

    public static func norm(_ v: UInt16) -> Double { Double(v) / 65535 }

    /// Whether anything is down: a contact, a button, a key, a gamepad off centre.
    public var busy: Bool {
        !fingers.isEmpty || pen?.touching == true || (pointer?.buttons ?? 0) != 0 || !held.isEmpty
            || pads.values.contains { $0 != Self.neutral($0.index) }
    }

    /// The events a datagram brings, or none when it is late, repeated or not ours. `now` in seconds.
    public mutating func apply(_ p: InputWire.Packet, now: TimeInterval) -> [InputEvent] {
        guard p.token == token, lastSeq.map({ InputWire.newer(p.seq, $0) }) ?? true else { return [] }
        lastSeq = p.seq
        lastRx = now
        var out: [InputEvent] = []
        for r in p.records {
            switch r {
            case let .touch(cs): touch(cs, &out)
            case let .pointer(ptr): pointer(ptr, &out)
            case let .keys(k): keys(k, &out)
            case let .gamepad(g): pad(g, &out)
            }
        }
        return out
    }

    /// Lets everything go when the receiver has fallen silent with something held.
    public mutating func watchdog(now: TimeInterval) -> [InputEvent] {
        guard let t = lastRx, now - t >= Self.watchdog, busy else { return [] }
        return releaseAll()
    }

    /// Everything up, the gamepads centred: the end of a session or a silent receiver.
    public mutating func releaseAll() -> [InputEvent] {
        var out: [InputEvent] = []
        touch([], &out)
        if var p = pointer {
            p.buttons = 0
            pointer(p, &out)
        }
        for code in held.sorted() { out.append(.key(code: code, down: false)) }
        held.removeAll()
        for i in pads.keys.sorted() { pad(Self.neutral(i), &out) }
        return out
    }

    private mutating func touch(_ cs: [InputWire.Contact], _ out: inout [InputEvent]) {
        var now: [UInt8: (UInt16, UInt16)] = [:]
        for c in cs where c.tool == .finger { now[c.id] = (c.x, c.y) }
        for id in fingers.keys.sorted() where now[id] == nil { out.append(.touchUp(slot: UInt32(id))) }
        for id in now.keys.sorted() {
            let raw = now[id]!
            let (slot, x, y) = (UInt32(id), Self.norm(raw.0), Self.norm(raw.1))
            if let old = fingers[id] {
                if old != raw { out.append(.touchMotion(slot: slot, x: x, y: y)) }
            } else {
                out.append(.touchDown(slot: slot, x: x, y: y))
            }
        }
        fingers = now
        // One stylus at a time: the first one listed.
        let next = cs.first { $0.tool != .finger }.map { c in
            InputEvent.Pen(x: Self.norm(c.x), y: Self.norm(c.y),
                           pressure: c.hover ? 0 : c.pressure == 0 ? 0.5 : Self.norm(c.pressure),
                           touching: !c.hover, eraser: c.tool == .eraser, button: c.button)
        }
        switch (pen, next) {
        case (.some, .none): out.append(.penOut)
        case let (old, .some(p)) where old != p: out.append(.pen(p))
        default: break
        }
        pen = next
    }

    private mutating func pointer(_ p: InputWire.Pointer, _ out: inout [InputEvent]) {
        guard let old = pointer else {
            // The first record only sets where the running totals start; its position and buttons still count.
            pointer = p
            if let a = p.abs { out.append(.pointerAbs(x: Self.norm(a.x), y: Self.norm(a.y))) }
            for (bit, code) in Self.buttons.enumerated() where p.buttons & 1 << bit != 0 {
                out.append(.button(code: code, down: true))
            }
            return
        }
        pointer = p
        if let a = p.abs, old.abs.map({ $0.x != a.x || $0.y != a.y }) ?? true {
            out.append(.pointerAbs(x: Self.norm(a.x), y: Self.norm(a.y)))
        }
        let (dx, dy) = (p.rel.x &- old.rel.x, p.rel.y &- old.rel.y)
        if dx != 0 || dy != 0 { out.append(.pointerRel(dx: Double(dx) / 16, dy: Double(dy) / 16)) }
        let (wy, wx) = (p.wheel.y &- old.wheel.y, p.wheel.x &- old.wheel.x)
        if p.smooth {
            if wy != 0 || wx != 0 {
                scrolling = true
                let px = { (v: Int32) in Double(v) / 120 * Self.pxPerNotch }
                out.append(.scroll(dx: px(wx), dy: px(wy), finish: false))
            } else if scrolling {
                // A repeat with the same totals: the fingers stopped or left.
                scrolling = false
                out.append(.scroll(dx: 0, dy: 0, finish: true))
            }
        } else if wy != 0 || wx != 0 {
            // Whole notches only; a high-resolution wheel's part-notches wait for the rest.
            let (ry, rx) = (wheelRest.y &+ wy, wheelRest.x &+ wx)
            wheelRest = (ry % 120, rx % 120)
            let (vertical, horizontal) = (ry / 120, rx / 120)
            if vertical != 0 || horizontal != 0 { out.append(.notches(vertical: vertical, horizontal: horizontal)) }
        }
        for (bit, code) in Self.buttons.enumerated() {
            let (was, now) = (old.buttons & 1 << bit != 0, p.buttons & 1 << bit != 0)
            if was != now { out.append(.button(code: code, down: now)) }
        }
    }

    private mutating func keys(_ k: InputWire.Keys, _ out: inout [InputEvent]) {
        for (i, e) in k.events.enumerated() {
            let n = k.first &+ UInt16(truncatingIfNeeded: i)
            if let a = keysApplied, !InputWire.newer16(n, a) { continue }
            keysApplied = n
            let changed = e.down ? held.insert(e.code).inserted : held.remove(e.code) != nil
            if changed { out.append(.key(code: e.code, down: e.down)) }
        }
        // Up events lost with every repeat of them: the held list lets those keys go.
        for code in held.sorted() where !k.held.contains(code) {
            held.remove(code)
            out.append(.key(code: code, down: false))
        }
    }

    private mutating func pad(_ g: InputWire.Pad, _ out: inout [InputEvent]) {
        guard g.connected else {
            if pads.removeValue(forKey: g.index) != nil { out.append(.padGone(g.index)) }
            return
        }
        if pads.updateValue(g, forKey: g.index) != g { out.append(.pad(g)) }
    }

    static func neutral(_ index: UInt8) -> InputWire.Pad { InputWire.Pad(index: index, connected: true) }
}
