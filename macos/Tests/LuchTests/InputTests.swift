// The receiver's input (PROTOCOL.md 4i): luchd's own vectors (input/wire.rs, state.rs, tests.rs), so both senders read
// the same datagrams alike, then the Mac's mapping, the permission and the whole way over a UDP pair.
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreGraphics
import Darwin
import XCTest
@testable import LuchEngine
@testable import LuchMedia
@testable import LuchNet

/// The receiver's side of the format: what luchd's tests encode with.
func encode(_ p: InputWire.Packet) -> [UInt8] {
    var out: [UInt8] = [InputWire.kind, InputWire.version, 0, 0]
    func be<T: FixedWidthInteger>(_ v: T, _ b: inout [UInt8]) { withUnsafeBytes(of: v.bigEndian) { b += $0 } }
    be(p.seq, &out)
    be(p.timeUS, &out)
    be(p.token, &out)
    for rec in p.records {
        var b: [UInt8] = []
        let type: UInt8
        switch rec {
        case let .touch(cs):
            b.append(UInt8(cs.count))
            for c in cs {
                b += [c.id, c.tool.rawValue | (c.button ? 0x40 : 0) | (c.hover ? 0x80 : 0)]
                for v in [c.x, c.y, c.pressure] { be(v, &b) }
            }
            type = InputWire.recTouch
        case let .pointer(p):
            b += [p.buttons, (p.abs != nil ? 1 : 0) | (p.smooth ? 2 : 0)]
            be(p.abs?.x ?? 0, &b)
            be(p.abs?.y ?? 0, &b)
            for v in [p.rel.x, p.rel.y, p.wheel.y, p.wheel.x] { be(v, &b) }
            type = InputWire.recPointer
        case let .keys(k):
            be(k.first, &b)
            b.append(UInt8(k.events.count))
            for e in k.events {
                be(e.code, &b)
                b.append(e.down ? 1 : 0)
            }
            b.append(UInt8(k.held.count))
            for c in k.held { be(c, &b) }
            type = InputWire.recKeys
        case let .gamepad(g):
            b += [g.index, g.connected ? 1 : 0]
            be(g.buttons, &b)
            for v in [g.left.x, g.left.y, g.right.x, g.right.y] { be(v, &b) }
            be(g.triggers.left, &b)
            be(g.triggers.right, &b)
            type = InputWire.recGamepad
        }
        out += [type, UInt8(b.count)] + b
    }
    return out
}

private func finger(_ id: UInt8, _ x: UInt16, _ y: UInt16) -> InputWire.Contact {
    InputWire.Contact(id: id, tool: .finger, x: x, y: y)
}

final class InputWireTests: XCTestCase {
    private func sample() -> InputWire.Packet {
        InputWire.Packet(seq: 7, timeUS: 123_456, token: 0x0123_4567_89ab_cdef, records: [
            .touch([finger(0, 0, 65535),
                    InputWire.Contact(id: 3, tool: .stylus, button: true, hover: true, x: 32768, y: 100,
                                      pressure: 4000)]),
            .pointer(InputWire.Pointer(buttons: 0b101, abs: (10, 20), smooth: true, rel: (-16, 32),
                                       wheel: (-120, 240))),
            .keys(InputWire.Keys(first: 65535, events: [(30, true), (30, false)], held: [42])),
            .gamepad(InputWire.Pad(index: 1, connected: true, buttons: 1 << 14, left: (-32768, 32767),
                                   right: (0, -1), triggers: (0, 65535))),
        ])
    }

    func testRoundTripsEveryRecord() throws {
        XCTAssertEqual(try InputWire.parse(encode(sample())), sample())
    }

    func testHeaderBytesAreAsDocumented() {
        let b = encode(InputWire.Packet(seq: 0x0102_0304, timeUS: 0x0a0b_0c0d, token: 0x1122_3344_5566_7788, records: []))
        XCTAssertEqual(b, [3, 0, 0, 0, 1, 2, 3, 4, 0x0a, 0x0b, 0x0c, 0x0d, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88])
        XCTAssertEqual(InputWire.token(of: b), 0x1122_3344_5566_7788)
    }

    func testTouchRecordBytesAreAsDocumented() {
        let b = encode(InputWire.Packet(seq: 0, token: 0, records: [
            .touch([InputWire.Contact(id: 2, tool: .eraser, hover: true, x: 0x1234, y: 0x5678, pressure: 0x9abc)]),
        ]))
        XCTAssertEqual(Array(b[InputWire.header...]), [1, 9, 1, 2, 0x83, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc])
    }

    func testUnknownRecordsAreSkippedAndBrokenOnesRefused() throws {
        var b = encode(sample())
        let known = try InputWire.parse(b).records.count
        b += [99, 3, 1, 2, 3]
        XCTAssertEqual(try InputWire.parse(b).records.count, known)
        b += [InputWire.recPointer, 30, 0] // says 30 bytes, has 1
        XCTAssertThrowsError(try InputWire.parse(b)) { XCTAssertEqual($0 as? InputWire.ParseError, .recordCutShort) }
        XCTAssertThrowsError(try InputWire.parse([3, 0, 0])) { XCTAssertEqual($0 as? InputWire.ParseError, .short) }
        var v = encode(sample())
        v[0] = 1
        XCTAssertThrowsError(try InputWire.parse(v)) { XCTAssertEqual($0 as? InputWire.ParseError, .notInput) }
        // A pointer record with too few bytes for its fields.
        let w = encode(InputWire.Packet(seq: 0, token: 0, records: [])) + [InputWire.recPointer, 2, 0, 0]
        XCTAssertThrowsError(try InputWire.parse(w)) { XCTAssertEqual($0 as? InputWire.ParseError, .badRecord) }
    }

    func testSequenceComparisonWraps() {
        XCTAssertTrue(InputWire.newer(1, 0))
        XCTAssertTrue(InputWire.newer(0, .max))
        XCTAssertFalse(InputWire.newer(5, 5))
        XCTAssertFalse(InputWire.newer(.max, 0))
        XCTAssertTrue(InputWire.newer16(2, 65534))
        XCTAssertFalse(InputWire.newer16(65534, 2))
    }
}

final class InputTrackerTests: XCTestCase {
    private let token: UInt64 = 42

    private func pkt(_ seq: UInt32, _ records: [InputWire.Record]) -> InputWire.Packet {
        InputWire.Packet(seq: seq, token: token, records: records)
    }

    func testMultitouchDownsMovesAndUpsFromStates() {
        var t = InputTracker(token: token)
        XCTAssertEqual(t.apply(pkt(1, [.touch([finger(0, 0, 0), finger(1, 65535, 65535)])]), now: 0),
                       [.touchDown(slot: 0, x: 0, y: 0), .touchDown(slot: 1, x: 1, y: 1)])
        // A repeat of the same state is nothing new.
        XCTAssertEqual(t.apply(pkt(2, [.touch([finger(0, 0, 0), finger(1, 65535, 65535)])]), now: 0), [])
        // Finger 0 lifted and finger 1 moved, in one lost-and-found state.
        XCTAssertEqual(t.apply(pkt(5, [.touch([finger(1, 0, 65535)])]), now: 0),
                       [.touchUp(slot: 0), .touchMotion(slot: 1, x: 0, y: 1)])
        XCTAssertTrue(t.busy)
        XCTAssertEqual(t.apply(pkt(6, [.touch([])]), now: 0), [.touchUp(slot: 1)])
        XCTAssertFalse(t.busy)
    }

    func testLateRepeatedAndForeignDatagramsDoNothing() {
        var t = InputTracker(token: token)
        XCTAssertEqual(t.apply(pkt(10, [.touch([finger(0, 1, 1)])]), now: 0).count, 1)
        XCTAssertEqual(t.apply(pkt(9, [.touch([])]), now: 0), [], "late")
        XCTAssertEqual(t.apply(pkt(10, [.touch([])]), now: 0), [], "repeated")
        var foreign = pkt(11, [.touch([])])
        foreign.token = 7
        XCTAssertEqual(t.apply(foreign, now: 0), [], "wrong token")
        XCTAssertEqual(t.apply(pkt(11, [.touch([])]), now: 0), [.touchUp(slot: 0)])
    }

    func testStylusPressureHoverAndLeaving() {
        var t = InputTracker(token: token)
        let pen = { (hover: Bool, pressure: UInt16) in
            InputWire.Contact(id: 0, tool: .stylus, button: true, hover: hover, x: 65535, y: 0, pressure: pressure)
        }
        XCTAssertEqual(t.apply(pkt(1, [.touch([pen(true, 0)])]), now: 0),
                       [.pen(.init(x: 1, y: 0, pressure: 0, touching: false, eraser: false, button: true))])
        XCTAssertFalse(t.busy, "hovering holds nothing")
        XCTAssertEqual(t.apply(pkt(2, [.touch([pen(false, 65535)])]), now: 0),
                       [.pen(.init(x: 1, y: 0, pressure: 1, touching: true, eraser: false, button: true))])
        XCTAssertEqual(t.apply(pkt(3, [.touch([])]), now: 0), [.penOut])
    }

    func testPointerRunningTotalsSurviveALostDatagram() {
        var t = InputTracker(token: token)
        let p = { (rel: (Int32, Int32), buttons: UInt8) in
            InputWire.Record.pointer(InputWire.Pointer(buttons: buttons, rel: rel))
        }
        XCTAssertEqual(t.apply(pkt(1, [p((160, 0), 0)]), now: 0), [], "the first record sets the base")
        // Datagram 2 (+16 px, left down) is lost; 3 has the totals and the button.
        XCTAssertEqual(t.apply(pkt(3, [p((160 + 512, -32), 1)]), now: 0),
                       [.pointerRel(dx: 32, dy: -2), .button(code: InputTracker.btnLeft, down: true)])
        XCTAssertEqual(t.apply(pkt(4, [p((672, -32), 0)]), now: 0),
                       [.button(code: InputTracker.btnLeft, down: false)])
        // Totals wrap around i32 without a jump.
        var w = InputTracker(token: token)
        _ = w.apply(pkt(1, [p((.max, 0), 0)]), now: 0)
        XCTAssertEqual(w.apply(pkt(2, [p((.min, 0), 0)]), now: 0), [.pointerRel(dx: 1.0 / 16, dy: 0)])
    }

    func testWheelNotchesAndSmoothScrolling() {
        var t = InputTracker(token: token)
        let w = { (wheel: (Int32, Int32), smooth: Bool) in
            InputWire.Record.pointer(InputWire.Pointer(smooth: smooth, wheel: wheel))
        }
        _ = t.apply(pkt(1, [w((0, 0), false)]), now: 0)
        XCTAssertEqual(t.apply(pkt(2, [w((60, 0), false)]), now: 0), [], "half a notch waits")
        XCTAssertEqual(t.apply(pkt(3, [w((240, -120), false)]), now: 0), [.notches(vertical: 2, horizontal: -1)])
        XCTAssertEqual(t.apply(pkt(4, [w((480, -120), true)]), now: 0),
                       [.scroll(dx: 0, dy: 2 * InputTracker.pxPerNotch, finish: false)])
        XCTAssertEqual(t.apply(pkt(5, [w((480, -120), true)]), now: 0), [.scroll(dx: 0, dy: 0, finish: true)])
        XCTAssertEqual(t.apply(pkt(6, [w((480, -120), true)]), now: 0), [])
    }

    func testKeyEventsApplyOnceAndHeldHealsLostUps() {
        var t = InputTracker(token: token)
        let k = { (first: UInt16, events: [(code: UInt16, down: Bool)], held: [UInt16]) in
            InputWire.Record.keys(InputWire.Keys(first: first, events: events, held: held))
        }
        XCTAssertEqual(t.apply(pkt(1, [k(65535, [(42, true), (30, true)], [42, 30])]), now: 0),
                       [.key(code: 42, down: true), .key(code: 30, down: true)])
        // The same events repeated, plus a new one numbered across the u16 wrap.
        XCTAssertEqual(t.apply(pkt(2, [k(65535, [(42, true), (30, true), (30, false)], [42])]), now: 0),
                       [.key(code: 30, down: false)])
        // Every repeat of shift's release was lost: the held list lets it go.
        XCTAssertEqual(t.apply(pkt(9, [k(10, [], [])]), now: 0), [.key(code: 42, down: false)])
        XCTAssertFalse(t.busy)
    }

    func testGamepadsChangeAndGo() {
        var t = InputTracker(token: token)
        let pad = InputWire.Pad(index: 0, connected: true, buttons: 1, left: (100, -100))
        XCTAssertEqual(t.apply(pkt(1, [.gamepad(pad)]), now: 0), [.pad(pad)])
        XCTAssertEqual(t.apply(pkt(2, [.gamepad(pad)]), now: 0), [])
        XCTAssertTrue(t.busy)
        XCTAssertEqual(t.apply(pkt(3, [.gamepad(InputWire.Pad(index: 0, connected: false))]), now: 0), [.padGone(0)])
    }

    func testWatchdogLetsGoAfterHalfASecondOfSilence() {
        var t = InputTracker(token: token)
        _ = t.apply(pkt(1, [
            .touch([finger(4, 1, 1)]),
            .pointer(InputWire.Pointer(buttons: 2)),
            .keys(InputWire.Keys(first: 0, events: [(29, true)], held: [29])),
            .gamepad(InputWire.Pad(index: 2, connected: true, triggers: (9, 0))),
        ]), now: 10)
        XCTAssertEqual(t.watchdog(now: 10.499), [])
        XCTAssertEqual(t.watchdog(now: 10 + InputTracker.watchdog), [
            .touchUp(slot: 4), .button(code: 0x111, down: false), .key(code: 29, down: false),
            .pad(InputTracker.neutral(2)),
        ])
        XCTAssertFalse(t.busy)
        XCTAssertEqual(t.watchdog(now: 12), [], "once")
    }
}

final class InputMapTests: XCTestCase {
    func testPointsOfThePictureAreDesktopPoints() {
        // The TV's virtual screen right of a 1512-point laptop panel.
        let g = InputGeometry(area: CGRect(x: 1512, y: 0, width: 1920, height: 1080))
        XCTAssertEqual(g.point(0, 0), CGPoint(x: 1512, y: 0))
        XCTAssertEqual(g.point(0.5, 0.25), CGPoint(x: 1512 + 960, y: 270))
        XCTAssertEqual(g.point(-0.2, 7), CGPoint(x: 1512, y: 1080), "outside the picture: its edge")
        XCTAssertEqual(g.point(.nan, 0), CGPoint(x: 1512, y: 0))
        XCTAssertTrue(g.areaSize == (1920, 1080))
        XCTAssertEqual(g.clamped(CGPoint(x: 0, y: 5000)), CGPoint(x: 1512, y: 1079))
    }

    func testAWindowIsFittedWithBarsAndFollowedWhereItIs() {
        // A 800x800 window in a 1920x1080 frame: bars left and right.
        let c = InputGeometry.letterbox(source: CGSize(width: 800, height: 800), frame: CGSize(width: 1920, height: 1080))
        XCTAssertEqual(c.minX, (1 - 1080.0 / 1920) / 2, accuracy: 1e-9)
        XCTAssertEqual(c.width, 1080.0 / 1920, accuracy: 1e-9)
        XCTAssertEqual(c.height, 1, accuracy: 1e-9)
        var at = CGRect(x: 100, y: 50, width: 800, height: 800)
        let g = { InputGeometry.of(.window(7), frame: CGSize(width: 1920, height: 1080), windowBounds: { _ in at }) }
        XCTAssertEqual(g().point(0.5, 0.5), CGPoint(x: 500, y: 450))
        XCTAssertEqual(g().point(0.05, 0), CGPoint(x: 100, y: 50), "a bar: the window's edge")
        at.origin = CGPoint(x: -300, y: 0) // moved onto a screen left of the main one
        XCTAssertEqual(g().point(0.5, 0.5), CGPoint(x: 100, y: 400))
    }

    func testACroppedDisplayAndALetterboxedOne() {
        let crop = InputGeometry.of(.area(CGRect(x: 76, y: 0, width: 1360, height: 765), letterboxed: false),
                                    frame: CGSize(width: 1920, height: 1080))
        XCTAssertEqual(crop.point(0, 0), CGPoint(x: 76, y: 0))
        XCTAssertEqual(crop.point(1, 1), CGPoint(x: 1436, y: 765))
        // A 16:10 panel letterboxed into 16:9: 1728 of the 1920 pixels, bars of 96 at the sides.
        let box = InputGeometry.of(.area(CGRect(x: 0, y: 0, width: 1440, height: 900), letterboxed: true),
                                   frame: CGSize(width: 1920, height: 1080))
        XCTAssertEqual(box.point(0.5, 0.5).x, 720, accuracy: 1e-9)
        XCTAssertEqual(box.point(0.05, 0).x, 0, accuracy: 1e-9)
        XCTAssertEqual(box.point(0.95, 1).x, 1440, accuracy: 1e-9)
        XCTAssertEqual(box.point(0.01, 1).x, 0, accuracy: 1e-9, "on a bar")
    }

    func testKeysGoByPositionAndModifiersHoldTheirFlags() {
        XCTAssertEqual(KeyMap.macKey(30), 0x00) // A
        XCTAssertEqual(KeyMap.macKey(16), 0x0C) // Q
        XCTAssertEqual(KeyMap.macKey(2), 0x12)  // 1
        XCTAssertEqual(KeyMap.macKey(11), 0x1D) // 0
        XCTAssertEqual(KeyMap.macKey(28), 0x24) // Enter
        XCTAssertEqual(KeyMap.macKey(14), 0x33) // Backspace is the Mac's Delete
        XCTAssertEqual(KeyMap.macKey(111), 0x75) // Delete is forward delete
        XCTAssertEqual(KeyMap.macKey(103), 0x7E) // Up
        XCTAssertEqual(KeyMap.macKey(59), 0x7A) // F1
        XCTAssertEqual(KeyMap.macKey(86), 0x0A) // the ISO key left of Z
        XCTAssertNil(KeyMap.macKey(0x110), "a mouse button is no key")
        XCTAssertEqual(KeyMap.macKey(125).flatMap(KeyMap.modifier), .maskCommand) // the Windows key is Command
        XCTAssertEqual(KeyMap.macKey(56).flatMap(KeyMap.modifier), .maskAlternate) // Alt is Option
        XCTAssertEqual(KeyMap.macKey(97).flatMap(KeyMap.modifier), .maskControl)
        XCTAssertNil(KeyMap.macKey(30).flatMap(KeyMap.modifier))
        XCTAssertEqual(KeyMap.systemKey(164), 16) // PlayPause
        XCTAssertEqual(KeyMap.systemKey(115), 0)  // VolumeUp
        XCTAssertNil(KeyMap.systemKey(30))
    }
}

final class InputInjectorTests: XCTestCase {
    private var posted: [MacInput] = []
    private var now = 100.0
    private var mouse = CGPoint(x: 5, y: 5)

    private func injector(_ area: CGRect = CGRect(x: 0, y: 0, width: 1920, height: 1080)) -> InputInjector {
        InputInjector(geometry: { InputGeometry(area: area) }, location: { [unowned self] in mouse },
                      post: { [unowned self] in posted.append($0) }, clock: { [unowned self] in now },
                      doubleClick: 0.5)
    }

    /// As luchd's desktop without a touch screen: the first finger is the pointer, the second is ignored.
    func testTheFirstFingerIsThePointer() {
        let i = injector()
        var t = InputTracker(token: 1)
        t.apply(InputWire.Packet(seq: 1, token: 1, records: [.touch([finger(5, 0, 0), finger(6, 65535, 65535)])]),
                now: 0).forEach(i.inject)
        t.apply(InputWire.Packet(seq: 2, token: 1, records: [.touch([])]), now: 0).forEach(i.inject)
        XCTAssertEqual(posted, [
            .move(to: .zero, delta: CGPoint(x: -5, y: -5), dragging: nil),
            .button(0, down: true, at: .zero, clicks: 1),
            .button(0, down: false, at: .zero, clicks: 1),
        ])
    }

    func testAFingerDragsAndTwoQuickTapsAreADoubleClick() {
        let i = injector()
        i.inject(.touchDown(slot: 0, x: 0.5, y: 0.5))
        i.inject(.touchMotion(slot: 0, x: 0.5, y: 0.6))
        XCTAssertEqual(posted.last, .move(to: CGPoint(x: 960, y: 648), delta: CGPoint(x: 0, y: 108), dragging: 0))
        i.inject(.touchUp(slot: 0))
        now += 0.2
        i.inject(.touchDown(slot: 1, x: 0.5, y: 0.6))
        XCTAssertEqual(posted.last, .button(0, down: true, at: CGPoint(x: 960, y: 648), clicks: 1),
                       "the last press was elsewhere: a click of its own")
        i.inject(.touchUp(slot: 1))
        now += 0.2
        i.inject(.touchDown(slot: 2, x: 0.5, y: 0.6))
        XCTAssertEqual(posted.last, .button(0, down: true, at: CGPoint(x: 960, y: 648), clicks: 2))
        i.inject(.touchUp(slot: 2))
        XCTAssertEqual(posted.last, .button(0, down: false, at: CGPoint(x: 960, y: 648), clicks: 2))
        now += 1
        i.inject(.touchDown(slot: 3, x: 0.5, y: 0.6))
        XCTAssertEqual(posted.last, .button(0, down: true, at: CGPoint(x: 960, y: 648), clicks: 1), "too late")
    }

    func testRelativeMotionStartsAtTheMacsPointerAndStaysOnThePicture() {
        let i = injector(CGRect(x: 1512, y: 0, width: 1920, height: 1080))
        mouse = CGPoint(x: 2000, y: 500)
        i.inject(.pointerRel(dx: 10, dy: -2))
        XCTAssertEqual(posted.last, .move(to: CGPoint(x: 2010, y: 498), delta: CGPoint(x: 10, y: -2), dragging: nil))
        // The system's own position lags: the next move goes on from where the last one put it.
        i.inject(.pointerRel(dx: 5000, dy: 0))
        XCTAssertEqual(posted.last, .move(to: CGPoint(x: 3431, y: 498), delta: CGPoint(x: 5000, y: 0), dragging: nil))
        // Later the Mac's own mouse has moved it: from there.
        now += 1
        mouse = CGPoint(x: 1600, y: 10)
        i.inject(.button(code: 0x111, down: true))
        XCTAssertEqual(posted.last, .button(1, down: true, at: CGPoint(x: 1600, y: 10), clicks: 1))
    }

    func testKeysCarryTheModifiersHeld() {
        let i = injector()
        i.inject(.key(code: 42, down: true))   // Shift
        i.inject(.key(code: 30, down: true))   // A
        i.inject(.key(code: 30, down: false))
        i.inject(.key(code: 42, down: false))
        i.inject(.key(code: 103, down: true))  // Up
        i.inject(.key(code: 164, down: true))  // PlayPause
        i.inject(.key(code: 0x2ff, down: true)) // nothing a Mac has
        XCTAssertEqual(posted, [
            .key(0x38, down: true, flags: .maskShift),
            .key(0x00, down: true, flags: .maskShift),
            .key(0x00, down: false, flags: .maskShift),
            .key(0x38, down: false, flags: []),
            .key(0x7E, down: true, flags: [.maskNumericPad, .maskSecondaryFn]),
            .system(16, down: true),
        ])
    }

    func testWheelsAndTouchpadScrolling() {
        let i = injector()
        i.inject(.notches(vertical: 2, horizontal: -1))
        i.inject(.scroll(dx: 0, dy: 7.5, finish: false))
        i.inject(.scroll(dx: 0, dy: 0.25, finish: false))
        i.inject(.scroll(dx: 0, dy: 0.5, finish: false))
        i.inject(.scroll(dx: 0, dy: 0, finish: true))
        XCTAssertEqual(posted, [
            .lines(vertical: 2, horizontal: -1),
            .pixels(dx: 0, dy: 7, phase: .began),
            .pixels(dx: 0, dy: 1, phase: .changed),
            .pixels(dx: 0, dy: 0, phase: .ended),
        ])
    }

    func testAStylusOnTheGlassIsATouchAndHoveringMovesThePointer() {
        let i = injector()
        let pen = { (touching: Bool) in
            InputEvent.Pen(x: 0.5, y: 0.5, pressure: touching ? 1 : 0, touching: touching, eraser: false, button: false)
        }
        i.inject(.pen(pen(false)))
        i.inject(.pen(pen(true)))
        i.inject(.pen(pen(false)))
        i.inject(.penOut)
        XCTAssertEqual(posted.map { if case .button = $0 { return "button" } else { return "move" } },
                       ["move", "move", "button", "button", "move"])
    }
}

final class InputPermissionTests: XCTestCase {
    func testThePermissionSteps() {
        var g = InputGate(canInject: true)
        XCTAssertNil(g.remembered(always: false))
        XCTAssertEqual(g.request(), .ask)
        XCTAssertEqual(g.request(), .wait, "one question at a time")
        XCTAssertEqual(g.answer(.once), .once)
        XCTAssertTrue(g.granted)
        XCTAssertEqual(g.request(), .reply(.once), "asked again in the session: the same answer")
        let m = InputGate.message(.once, token: 0xab, area: (1920, 1080))
        XCTAssertEqual(m["grant"] as? String, "once")
        XCTAssertEqual(m["token"] as? String, "00000000000000ab")
        XCTAssertEqual(m["accept"] as? [String], ["touch", "pointer", "keyboard"])
        XCTAssertEqual(m["area"] as? [Int], [1920, 1080])

        var no = InputGate(canInject: true)
        XCTAssertEqual(no.request(), .ask)
        no.answer(.no)
        XCTAssertFalse(no.granted)
        XCTAssertEqual(no.request(), .reply(.no), "no holds for the session")
        XCTAssertEqual(InputGate.message(.no, token: 1, area: (1, 1)) as NSDictionary, ["t": "input", "grant": "no"])

        // "Always" before: granted at the start, no question.
        var always = InputGate(canInject: true)
        XCTAssertEqual(always.remembered(always: true), .always)
        XCTAssertEqual(always.request(), .reply(.always))

        // Nothing to inject with: no, without asking, even after an "always".
        var nothing = InputGate(canInject: false)
        XCTAssertNil(nothing.remembered(always: true))
        XCTAssertEqual(nothing.request(), .reply(.no))
    }
}

/// The whole way over a UDP pair, as luchd's tests.rs: the request, the person's answer, the grant, the datagrams on
/// the video flow, the gate, the tracker and the injector.
final class InputFlowTests: XCTestCase {
    private final class Box<T> {
        private let lock = NSLock()
        private var items: [T] = []
        func add(_ v: T) { lock.lock(); items.append(v); lock.unlock() }
        var all: [T] { lock.lock(); defer { lock.unlock() }; return items }
    }

    /// The receiver's video socket and the sender's connected one; the first video datagram tells the receiver where
    /// to answer.
    private func flow() -> (sender: UdpSender, receiver: Int32, to: sockaddr_in) {
        let r = socket(AF_INET, SOCK_DGRAM, 0)
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        a.sin_addr.s_addr = inet_addr("127.0.0.1")
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        withUnsafeMutablePointer(to: &a) { p in
            p.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                _ = Darwin.bind(r, $0, len)
                _ = getsockname(r, $0, &len)
            }
        }
        var tv = timeval(tv_sec: 2, tv_usec: 0)
        setsockopt(r, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        let s = UdpSender.open(kind: .video, ip: "127.0.0.1", port: UInt16(bigEndian: a.sin_port), interfaceIndex: 0,
                               dscp: 0)!
        s.send(frame: [1, 2, 3], flags: [], ptsUS: 0, hostLat: 0)
        var from = sockaddr_in()
        var flen = socklen_t(MemoryLayout<sockaddr_in>.size)
        var buf = [UInt8](repeating: 0, count: 2048)
        withUnsafeMutablePointer(to: &from) { p in
            p.withMemoryRebound(to: sockaddr.self, capacity: 1) { _ = recvfrom(r, &buf, buf.count, 0, $0, &flen) }
        }
        return (s, r, from)
    }

    private func send(_ p: InputWire.Packet, from r: Int32, to: sockaddr_in) {
        var to = to
        let b = encode(p)
        withUnsafePointer(to: &to) { p in
            p.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                _ = Darwin.sendto(r, b, b.count, 0, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
    }

    private func wait(_ ok: () -> Bool) {
        let until = Date().addingTimeInterval(2)
        while !ok(), Date() < until { Thread.sleep(forTimeInterval: 0.02) }
    }

    func testRequestAnswerGrantAndInjection() throws {
        let (video, r, to) = flow()
        defer { close(r) }
        let posted = Box<MacInput>(), sent = Box<[String: Any]>(), asked = Box<String>(), kept = Box<InputAnswer>()
        let control = InputControl(always: false, ask: { name, devices, reply in
            asked.add("\(name): \(devices.joined(separator: ","))")
            reply(.once)
        }, remember: kept.add, permission: InputPermission(allowed: { true }, request: {}),
           post: posted.add, location: { .zero })
        control.welcome(["t": "welcome", "input": ["touch", "keyboard"]], name: "Living room TV")
        control.started(video: video, geometry: { InputGeometry(area: CGRect(x: 1920, y: 0, width: 1920, height: 1080)) },
                        send: sent.add)
        XCTAssertTrue(sent.all.isEmpty, "nothing granted before a request")

        // Before the grant nothing is injected (the token is not known to the receiver yet, but even with it).
        XCTAssertTrue(control.handle(["t": "input_request", "devices": ["touch"]]))
        wait { !sent.all.isEmpty }
        XCTAssertEqual(asked.all, ["Living room TV: touch"])
        XCTAssertTrue(kept.all.isEmpty, "once is not remembered")
        let grant = try XCTUnwrap(sent.all.first)
        XCTAssertEqual(grant["grant"] as? String, "once")
        XCTAssertEqual(grant["area"] as? [Int], [1920, 1080])
        let token = try XCTUnwrap(UInt64(grant["token"] as? String ?? "", radix: 16))

        // A wrong token is dropped; the right one lands on the TV's part of the desktop.
        send(InputWire.Packet(seq: 1, token: token ^ 1, records: [.touch([finger(0, 65535, 65535)])]), from: r, to: to)
        send(InputWire.Packet(seq: 2, token: token, records: [.touch([finger(0, 32768, 0)])]), from: r, to: to)
        send(InputWire.Packet(seq: 3, token: token, records: [.touch([])]), from: r, to: to)
        wait { posted.all.count >= 3 }
        let x = 1920 + 32768.0 / 65535 * 1920
        XCTAssertEqual(posted.all, [
            .move(to: CGPoint(x: x, y: 0), delta: CGPoint(x: x, y: 0), dragging: nil),
            .button(0, down: true, at: CGPoint(x: x, y: 0), clicks: 1),
            .button(0, down: false, at: CGPoint(x: x, y: 0), clicks: 1),
        ])

        // Shift held when the receiver falls silent: the watchdog lets it go.
        send(InputWire.Packet(seq: 4, token: token, records: [.keys(InputWire.Keys(first: 0, events: [(42, true)],
                                                                                   held: [42]))]), from: r, to: to)
        wait { posted.all.count >= 5 }
        XCTAssertEqual(Array(posted.all.suffix(2)), [.key(0x38, down: true, flags: .maskShift),
                                                     .key(0x38, down: false, flags: [])])

        // Taken back: the receiver is told, nothing more is injected, a new request is answered no.
        control.takeBack()
        wait { sent.all.count >= 2 }
        XCTAssertEqual(sent.all[1] as NSDictionary, ["t": "input", "grant": "off"])
        let before = posted.all.count
        send(InputWire.Packet(seq: 5, token: token, records: [.touch([finger(3, 1, 1)])]), from: r, to: to)
        _ = control.handle(["t": "input_request"])
        wait { sent.all.count >= 3 }
        XCTAssertEqual(sent.all[2]["grant"] as? String, "no")
        XCTAssertEqual(posted.all.count, before)
        control.ended()
    }

    func testAlwaysIsGrantedAtTheStartAndANewPictureGetsANewToken() throws {
        let (video, r, _) = flow()
        defer { close(r) }
        let sent = Box<[String: Any]>()
        let control = InputControl(always: true, ask: { _, _, _ in XCTFail("not asked") }, remember: { _ in },
                                   permission: InputPermission(allowed: { true }, request: {}),
                                   post: { _ in }, location: { .zero })
        control.welcome(["t": "welcome", "input": ["keyboard"]], name: "TV")
        let geometry = { InputGeometry(area: CGRect(x: 0, y: 0, width: 1280, height: 720)) }
        control.started(video: video, geometry: geometry, send: sent.add)
        XCTAssertEqual(sent.all.first?["grant"] as? String, "always")
        XCTAssertTrue(control.granted)
        // The screen switched: a new flow, the same grant under a new token.
        control.ended()
        control.started(video: video, geometry: geometry, send: sent.add)
        XCTAssertEqual(sent.all.count, 2)
        XCTAssertNotEqual(sent.all[0]["token"] as? String, sent.all[1]["token"] as? String)
        control.ended()
    }

    func testWithoutMacOSsPermissionTheAnswerIsNoAndAReceiverWithoutInputIsNeverGranted() throws {
        let (video, r, _) = flow()
        defer { close(r) }
        let sent = Box<[String: Any]>(), requested = Box<Bool>()
        let control = InputControl(always: false, ask: { _, _, reply in reply(.always) }, remember: { _ in },
                                   permission: InputPermission(allowed: { false }, request: { requested.add(true) }),
                                   permissionWait: 0.1, post: { _ in }, location: { .zero })
        control.welcome(["t": "welcome", "input": ["touch"]], name: "TV")
        control.started(video: video, geometry: { InputGeometry(area: .zero) }, send: sent.add)
        _ = control.handle(["t": "input_request"])
        wait { !sent.all.isEmpty }
        XCTAssertEqual(requested.all, [true], "macOS was asked")
        XCTAssertEqual(sent.all.first as NSDictionary?, ["t": "input", "grant": "no"])
        control.ended()

        let quiet = InputControl(always: true, ask: { _, _, _ in XCTFail("not asked") }, remember: { _ in },
                                 permission: InputPermission(allowed: { true }, request: {}),
                                 post: { _ in }, location: { .zero })
        quiet.welcome(["t": "welcome"], name: "TV")
        let none = Box<[String: Any]>()
        quiet.started(video: video, geometry: { InputGeometry(area: .zero) }, send: none.add)
        XCTAssertTrue(none.all.isEmpty, "no grant up front")
        _ = quiet.handle(["t": "input_request"])
        wait { !none.all.isEmpty }
        XCTAssertEqual(none.all.first as NSDictionary?, ["t": "input", "grant": "no"])
        quiet.ended()
    }
}
