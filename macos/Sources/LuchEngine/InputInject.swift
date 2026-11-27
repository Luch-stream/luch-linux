// Puts the receiver's input on this Mac as the events a mouse and a keyboard plugged into it would make (CGEvent).
// A Mac has no touch screen: the first finger (or a stylus on the glass) is the pointer with its left button, as luchd
// does where the desktop has no touch device. A gamepad cannot be made without a DriverKit driver, so none is accepted.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import CoreGraphics
import Foundation

/// What is posted, before it becomes a CGEvent: the injector's decisions, which the tests read.
public enum MacInput: Equatable {
    /// The pointer to `to`; `dragging` is the button held (0 left, 1 right, 2.. others), `delta` the motion.
    case move(to: CGPoint, delta: CGPoint, dragging: Int?)
    case button(Int, down: Bool, at: CGPoint, clicks: Int)
    /// Wheel notches, positive down / right as on the wire.
    case lines(vertical: Int32, horizontal: Int32)
    /// Touchpad scrolling in whole points, positive down / right; `phase` as a trackpad's (began, changed, ended).
    case pixels(dx: Int32, dy: Int32, phase: ScrollPhase)
    case key(UInt16, down: Bool, flags: CGEventFlags)
    /// NX_KEYTYPE_* (MediaKeys).
    case system(Int32, down: Bool)

    public enum ScrollPhase: Int64 {
        case began = 1, changed = 2, ended = 4
    }
}

public final class InputInjector {
    /// The grant's `accept` (PROTOCOL.md 4i): a touch becomes the pointer, a keyboard is a keyboard.
    public static let accepts = ["touch", "pointer", "keyboard"]
    /// The touch slot a stylus takes where it can only be a touch.
    static let penSlot: UInt32 = 15

    private let geometry: () -> InputGeometry
    private let location: () -> CGPoint
    private let post: (MacInput) -> Void
    private let clock: () -> TimeInterval
    private let doubleClick: TimeInterval

    private var cursor: CGPoint?
    private var movedAt: TimeInterval = -1
    private var held: [Int] = []
    private var lastDown: (button: Int, at: CGPoint, time: TimeInterval, count: Int)?
    private var flags: CGEventFlags = []
    private var capsLock = false
    private var pointerFinger: UInt32?
    private var penAsTouch = false
    private var scrolling = false
    private var scrollRest = (x: 0.0, y: 0.0)

    /// `geometry` is asked at every event (a window moves), `location` is the Mac's own pointer, `post` sends.
    public init(geometry: @escaping () -> InputGeometry, location: @escaping () -> CGPoint,
                post: @escaping (MacInput) -> Void, clock: @escaping () -> TimeInterval,
                doubleClick: TimeInterval = NSEvent.doubleClickInterval) {
        self.geometry = geometry
        self.location = location
        self.post = post
        self.clock = clock
        self.doubleClick = doubleClick
    }

    public func inject(_ e: InputEvent) {
        switch e {
        case let .touchDown(slot, x, y): touch(slot, at: (x, y), down: true)
        case let .touchMotion(slot, x, y): touch(slot, at: (x, y), down: false)
        case let .touchUp(slot): touch(slot, at: nil, down: false)
        case let .pen(p):
            // No pen device: touching is a touch, hovering moves the pointer.
            if p.touching {
                let down = !penAsTouch
                penAsTouch = true
                touch(Self.penSlot, at: (p.x, p.y), down: down)
            } else {
                penUp()
                moveTo(geometry().point(p.x, p.y))
            }
        case .penOut: penUp()
        case let .pointerAbs(x, y): moveTo(geometry().point(x, y))
        case let .pointerRel(dx, dy):
            let from = here()
            let to = geometry().clamped(CGPoint(x: from.x + dx, y: from.y + dy))
            moveTo(to, delta: CGPoint(x: dx, y: dy))
        case let .button(code, down): button(Self.mouseButton(code), down: down)
        case let .notches(v, h): post(.lines(vertical: v, horizontal: h))
        case let .scroll(dx, dy, finish): scroll(dx, dy, finish: finish)
        case let .key(code, down): key(code, down: down)
        case .pad, .padGone: break
        }
    }

    /// CGEvent's button number for an evdev BTN_* code: left 0, right 1, middle 2, back 3, forward 4.
    static func mouseButton(_ code: UInt16) -> Int {
        switch code {
        case 0x110: return 0
        case 0x111: return 1
        case 0x112: return 2
        case 0x116: return 3
        case 0x115: return 4
        default: return Int(code) & 0x1f
        }
    }

    /// Where the pointer is: where it was put a moment ago (the system's own position lags behind a burst of moves),
    /// else wherever the Mac's own mouse has taken it.
    private func here() -> CGPoint {
        if let cursor, clock() - movedAt < 0.25 { return cursor }
        return location()
    }

    private func moveTo(_ p: CGPoint, delta: CGPoint? = nil) {
        let from = here()
        cursor = p
        movedAt = clock()
        post(.move(to: p, delta: delta ?? CGPoint(x: p.x - from.x, y: p.y - from.y), dragging: held.min()))
    }

    private func button(_ b: Int, down: Bool) {
        let at = here()
        var clicks = 1
        if down {
            let now = clock()
            if let l = lastDown, l.button == b, now - l.time <= doubleClick, abs(l.at.x - at.x) < 5,
               abs(l.at.y - at.y) < 5 {
                clicks = l.count + 1
            }
            lastDown = (b, at, now, clicks)
            if !held.contains(b) { held.append(b) }
        } else {
            guard let i = held.firstIndex(of: b) else { return }
            held.remove(at: i)
            if let l = lastDown, l.button == b { clicks = l.count }
        }
        post(.button(b, down: down, at: at, clicks: clicks))
    }

    private func touch(_ slot: UInt32, at: (Double, Double)?, down: Bool) {
        // The first finger is the pointer, the others are ignored.
        if pointerFinger == nil, down { pointerFinger = slot }
        guard pointerFinger == slot else { return }
        if let (x, y) = at {
            moveTo(geometry().point(x, y))
            if down { button(0, down: true) }
        } else {
            pointerFinger = nil
            button(0, down: false)
        }
    }

    private func penUp() {
        if penAsTouch {
            penAsTouch = false
            touch(Self.penSlot, at: nil, down: false)
        }
    }

    private func scroll(_ dx: Double, _ dy: Double, finish: Bool) {
        if finish {
            scrolling = false
            scrollRest = (0, 0)
            post(.pixels(dx: 0, dy: 0, phase: .ended))
            return
        }
        // Whole points: the parts wait for the next move.
        let x = scrollRest.x + dx, y = scrollRest.y + dy
        let (ix, iy) = (x.rounded(.towardZero), y.rounded(.towardZero))
        scrollRest = (x - ix, y - iy)
        guard ix != 0 || iy != 0 else { return }
        post(.pixels(dx: Int32(ix), dy: Int32(iy), phase: scrolling ? .changed : .began))
        scrolling = true
    }

    private func key(_ code: UInt16, down: Bool) {
        if let nx = KeyMap.systemKey(code) {
            post(.system(nx, down: down))
            return
        }
        guard let k = KeyMap.macKey(code) else { return }
        if let m = KeyMap.modifier(k) {
            if down { flags.insert(m) } else { flags.remove(m) }
        } else if k == KeyMap.capsLock, down {
            capsLock.toggle()
        }
        var f = flags.union(KeyMap.ownFlags(k))
        if capsLock { f.insert(.maskAlphaShift) }
        post(.key(k, down: down, flags: f))
    }
}

/// Posts what the injector decided, as the HID system would: the Mac's own mouse keeps working meanwhile.
public enum InputPoster {
    private static let source: CGEventSource? = {
        let s = CGEventSource(stateID: .hidSystemState)
        s?.localEventsSuppressionInterval = 0
        return s
    }()

    /// The Mac's pointer now, in global coordinates.
    public static func location() -> CGPoint { CGEvent(source: nil)?.location ?? .zero }

    public static func post(_ m: MacInput) {
        switch m {
        case let .move(to, delta, dragging):
            let type: CGEventType = switch dragging {
            case nil: .mouseMoved
            case 0: .leftMouseDragged
            case 1: .rightMouseDragged
            default: .otherMouseDragged
            }
            guard let e = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: to,
                                  mouseButton: CGMouseButton(rawValue: UInt32(dragging ?? 0)) ?? .left) else { return }
            e.setIntegerValueField(.mouseEventDeltaX, value: Int64(delta.x.rounded()))
            e.setIntegerValueField(.mouseEventDeltaY, value: Int64(delta.y.rounded()))
            e.post(tap: .cghidEventTap)
        case let .button(b, down, at, clicks):
            let type: CGEventType = switch (b, down) {
            case (0, true): .leftMouseDown
            case (0, false): .leftMouseUp
            case (1, true): .rightMouseDown
            case (1, false): .rightMouseUp
            case (_, true): .otherMouseDown
            case (_, false): .otherMouseUp
            }
            guard let e = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: at,
                                  mouseButton: CGMouseButton(rawValue: UInt32(b)) ?? .center) else { return }
            e.setIntegerValueField(.mouseEventClickState, value: Int64(clicks))
            e.setIntegerValueField(.mouseEventButtonNumber, value: Int64(b))
            e.post(tap: .cghidEventTap)
        case let .lines(v, h):
            // CoreGraphics counts up / left as positive.
            CGEvent(scrollWheelEvent2Source: source, units: .line, wheelCount: 2, wheel1: -v, wheel2: -h, wheel3: 0)?
                .post(tap: .cghidEventTap)
        case let .pixels(dx, dy, phase):
            guard let e = CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2, wheel1: -dy,
                                  wheel2: -dx, wheel3: 0) else { return }
            e.setIntegerValueField(.scrollWheelEventIsContinuous, value: 1)
            e.setIntegerValueField(.scrollWheelEventScrollPhase, value: phase.rawValue)
            e.post(tap: .cghidEventTap)
        case let .key(k, down, flags):
            guard let e = CGEvent(keyboardEventSource: source, virtualKey: k, keyDown: down) else { return }
            if KeyMap.modifier(k) != nil || k == KeyMap.capsLock { e.type = .flagsChanged }
            e.flags = flags
            e.post(tap: .cghidEventTap)
        case let .system(code, down):
            MediaKeys.postSystem(code, down: down)
        }
    }

    /// Whether macOS lets this app post events (Privacy & Security, Accessibility).
    public static var allowed: Bool { CGPreflightPostEventAccess() }

    /// Shows macOS's own request for that, once; true when it is allowed already.
    @discardableResult
    public static func requestAccess() -> Bool { CGRequestPostEventAccess() }
}
