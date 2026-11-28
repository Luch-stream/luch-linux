// A whole calibration against fake receivers (GroupTests' FakeSyncReceiver) and a fake microphone in a made-up room:
// each receiver answers its probe as the real one does (played = pts + D + its offset, by its own reckoning), the
// room adds what lies after each one's DAC (hidden from the sender) and the microphone's own latency, and the
// corrections must make every receiver heard at the same moment.
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreMedia
import Darwin
import Foundation
import XCTest
@testable import LuchEngine
@testable import LuchProto

/// The room: who sounded when, and a microphone that records it with its own crystal, latency and buffer jitter.
private final class FakeRoom: CalibrationMic {
    struct Sweep {
        let hostStart: Double
        let gain: Double
    }

    let rate: Double
    /// The microphone's crystal against the host's, and what it adds before a buffer is time-stamped.
    let drift = 60e-6
    let micLatency = 0.009
    private let lock = NSLock()
    private var sweeps: [Sweep] = []
    private var began: Double?
    private(set) var opened = false

    init(rate: Double) { self.rate = rate }

    /// A sweep's first sample reached the air at this host time.
    func sound(atHost t: Double, gain: Double) {
        lock.lock()
        sweeps.append(Sweep(hostStart: t, gain: gain))
        lock.unlock()
    }

    func start() throws {
        opened = true
        began = hostNow()
    }

    func recording() -> MicRecording {
        guard let start = began else { return MicRecording(samples: [], rate: rate, anchors: []) }
        let real = rate * (1 + drift)
        let count = Int((hostNow() - start) * real)
        let r = JavaRandom(9)
        var rec = (0..<count).map { _ in Float(0.002 * r.nextGaussian()) }
        lock.lock()
        let all = sweeps
        lock.unlock()
        for s in all {
            // Heard: the direct sound and a wall 4 ms later, louder; as captured, the microphone's latency later.
            for (delay, g) in [(0.0, 0.5), (0.004, 0.8)] {
                let at = (s.hostStart + delay + micLatency - start) * real
                let first = max(0, Int(at.rounded(.up)))
                for i in first..<min(count, first + Int(0.21 * rate)) {
                    rec[i] += Float(g * s.gain * SimRoom.sweepAt((Double(i) - at) / real))
                }
            }
        }
        let anchors = stride(from: 0, to: count, by: 512).map { i in
            (sample: i, host: start + Double(i) / real + 0.0003 * (r.nextDouble() - 0.5))
        }
        return MicRecording(samples: rec, rate: rate, anchors: anchors)
    }

    func stop() {}

    func hostNow() -> Double { CMTimeGetSeconds(CMClockGetTime(CMClockGetHostTimeClock())) }
}

final class CalibrateGroupTests: XCTestCase {
    private var store: SyncStore!

    override func setUp() {
        signal(SIGPIPE, SIG_IGN)
        // A store of its own each time: a domain removed and opened again may still hold the last run's values.
        let suite = "luch.tests.calibrate.\(UUID().uuidString)"
        suiteName = suite
        store = SyncStore(defaults: UserDefaults(suiteName: suite)!)
    }

    private var suiteName = ""

    override func tearDown() {
        UserDefaults(suiteName: suiteName)?.removePersistentDomain(forName: suiteName)
    }

    private func options() -> MirrorOptions {
        var o = MirrorOptions()
        o.synthetic = true
        o.width = 640
        o.height = 360
        o.explicitSize = true
        return o
    }

    /// A receiver that plays its probe's sweeps at pts + D + its offset (+ `late`, its own path's error, which it
    /// sees and reports), and whose sound reaches the air `hidden` later still: the part only a microphone hears.
    private func plays(_ rx: FakeSyncReceiver, hidden: Double, late: Double = 0, quietBelow: Double = 0,
                       room: FakeRoom, group: @escaping () -> GroupSession?) {
        var offset = rx.route == "hdmi_arc" ? 40.0 : 0
        rx.answer = { m in
            switch m["t"] as? String {
            case "offset":
                offset = (m["ms"] as? NSNumber)?.doubleValue ?? offset
                return nil
            case "probe":
                guard let g = group() else { return nil }
                let at = (m["at"] as? NSNumber)?.int64Value ?? 0
                let (n, ms, gap) = (m["n"] as? Int ?? 0, m["ms"] as? Int ?? 0, m["gap_ms"] as? Int ?? 0)
                let gain = min(0.5, (m["gain"] as? NSNumber)?.doubleValue ?? 0)
                let d = g.delay.audio
                let played = (0..<n).map { i in
                    at + Int64(i * (ms + gap)) * 1000 + Int64(((d + offset + late) * 1000).rounded())
                }
                // A speaker the microphone hears only when it is louder (the retry).
                if gain >= quietBelow {
                    for p in played { room.sound(atHost: g.clockZero + Double(p) / 1e6 + hidden / 1000, gain: gain) }
                }
                return ["t": "probe_done", "id": m["id"] ?? "", "played": played, "src": "timestamp"]
            default:
                return nil
            }
        }
    }

    func testThreeReceiversHeardApartEndUpHeardTogether() throws {
        let room = FakeRoom(rate: 44100)
        let kitchen = FakeSyncReceiver(name: "Kitchen", needMs: 20)
        let den = FakeSyncReceiver(name: "Den", needMs: 20, route: "hdmi_arc")
        let hall = FakeSyncReceiver(name: "Hall", needMs: 20)
        var group: GroupSession?
        // What each adds after its DAC: none, a soundbar's 25 ms, a projector's 60 ms. Den also starts at the
        // hdmi_arc preset (40 ms), which its played times include; Hall's own path runs 3 ms late, which it sees.
        let hidden = ["kitchen": 0.0, "den": 25.0, "hall": 60.0]
        plays(kitchen, hidden: 0, room: room) { group }
        plays(den, hidden: 25, room: room) { group }
        plays(hall, hidden: 60, late: 3, quietBelow: 0.4, room: room) { group }
        let g = GroupSession(members: [GroupMember(kitchen.receiver, role: .both), GroupMember(den.receiver, role: .audio),
                                       GroupMember(hall.receiver, role: .audio)],
                             mode: .watching, soundOnly: true, options: options(), store: store)
        group = g
        try g.start()
        defer { g.stop(reason: "test") }
        // D from the first reports, as a calibration finds it in a group that plays.
        XCTAssertTrue(waitUntil { g.delay.audio == 150 && den.received("offset").first != nil })

        let before = CMTimeGetSeconds(CMClockGetTime(CMClockGetHostTimeClock()))
        let out = try Calibrator(group: g, mic: room, store: store, signal: ProbeSignal(n: 2)).run()
        XCTAssertTrue(room.opened)

        // The probes: each member its own turn, the first past now + D + 1 s, on the group's pts clock.
        let firsts = [kitchen, den, hall].map { ($0.received("probe").first?["at"] as? NSNumber)?.int64Value ?? 0 }
        let nowUS = Int64((before - g.clockZero) * 1e6)
        XCTAssertGreaterThanOrEqual(firsts[0], nowUS + 1_150_000)
        let slot = ProbeSignal(n: 2).slotUS
        XCTAssertEqual(firsts[1] - firsts[0], slot)
        XCTAssertEqual(firsts[2] - firsts[0], 2 * slot)
        XCTAssertEqual(kitchen.received("probe").first?["gain"] as? Double, 0.3)
        // Hall was not heard at 0.3: asked once more, louder, alone.
        XCTAssertEqual(hall.received("probe").count, 2)
        XCTAssertEqual(hall.received("probe").last?["gain"] as? Double, Calibration.retryGain)
        XCTAssertEqual(kitchen.received("probe").count, 1)

        XCTAssertEqual(out.map(\.measured), [true, true, true], out.map(\.why).joined(separator: "; "))
        XCTAssertEqual(out.map(\.retried), [false, false, true])
        // heard − played: what lies after the DAC plus the microphone's latency.
        for o in out {
            XCTAssertEqual(o.delayMs ?? 0, hidden[o.id]! + room.micLatency * 1000, accuracy: 0.3, o.name)
            XCTAssertGreaterThanOrEqual(o.psrDb ?? 0, AcousticProbe.minPsrDb)
            XCTAssertLessThanOrEqual(o.spreadMs ?? 1, AcousticProbe.agreeMs)
        }
        // Absolute offsets: everyone as late as the projector, then heard together within half a millisecond.
        let offsets = Dictionary(uniqueKeysWithValues: out.map { ($0.id, $0.offsetMs) })
        XCTAssertEqual(offsets["hall"]!, 0, accuracy: 0.3)
        let heardAt = hidden.map { $0.value + offsets[$0.key]! }
        XCTAssertLessThanOrEqual(heardAt.max()! - heardAt.min()!, 0.5, "\(offsets)")

        // Sent to each at once, and kept for its output route.
        for (rx, id) in [(kitchen, "kitchen"), (den, "den"), (hall, "hall")] {
            XCTAssertTrue(waitUntil { rx.received("offset").last?["ms"] as? Double == offsets[id] }, id)
            XCTAssertEqual(store.offset(Sync.offsetKey(id, route: rx.route)), offsets[id])
        }
    }

    func testAReceiverThatRefusesIsNotMeasuredAndKeepsItsOffset() throws {
        let room = FakeRoom(rate: 48000)
        let kitchen = FakeSyncReceiver(name: "Kitchen", needMs: 20)
        let den = FakeSyncReceiver(name: "Den", needMs: 20, route: "hdmi_arc")
        let hall = FakeSyncReceiver(name: "Hall", needMs: 20)
        var group: GroupSession?
        plays(kitchen, hidden: 0, room: room) { group }
        plays(hall, hidden: 30, room: room) { group }
        den.answer = { m in
            m["t"] as? String == "probe"
                ? ["t": "error", "for": "probe", "id": m["id"] ?? "", "code": "probe_off", "detail": "off"] : nil
        }
        let g = GroupSession(members: [GroupMember(kitchen.receiver, role: .both), GroupMember(den.receiver, role: .audio),
                                       GroupMember(hall.receiver, role: .audio)],
                             mode: .watching, soundOnly: true, options: options(), store: store)
        group = g
        try g.start()
        defer { g.stop(reason: "test") }
        XCTAssertTrue(waitUntil { g.delay.audio == 150 && den.received("offset").first != nil })

        let out = try Calibrator(group: g, mic: room, store: store, signal: ProbeSignal(n: 2)).run()
        XCTAssertEqual(out.map(\.measured), [true, false, true])
        XCTAssertEqual(out[1].why, "it plays no sound here, or calibration is off in its settings")
        XCTAssertEqual(out[1].offsetMs, 0, "nothing stored for it: it keeps what it plays with")
        XCTAssertFalse(out[1].retried, "a refusal is not asked again")
        XCTAssertEqual(den.received("probe").count, 1)
        XCTAssertEqual(out[0].offsetMs, 30, accuracy: 0.3)
        XCTAssertEqual(out[2].offsetMs, 0, accuracy: 0.3)
        XCTAssertNil(store.offset(Sync.offsetKey("den", route: "hdmi_arc")))
    }

    func testOneSoundingReceiverIsNotEnough() throws {
        let tv = FakeSyncReceiver(name: "TV", needMs: 20)
        let speaker = FakeSyncReceiver(name: "Speaker", needMs: 20)
        let room = FakeRoom(rate: 48000)
        let g = GroupSession(members: [GroupMember(tv.receiver, role: .video), GroupMember(speaker.receiver, role: .audio)],
                             mode: .interactive, soundOnly: false, options: options(), store: store)
        try g.start()
        defer { g.stop(reason: "test") }
        XCTAssertThrowsError(try Calibrator(group: g, mic: room, store: store).run()) { e in
            XCTAssertEqual(e.localizedDescription, "calibration needs two or more receivers playing sound in the group")
        }
        XCTAssertFalse(room.opened, "the microphone is not opened for nothing")
    }
}

private func waitUntil(_ seconds: Double = 4, _ check: () -> Bool) -> Bool {
    let until = Date().addingTimeInterval(seconds)
    while Date() < until {
        if check() { return true }
        Thread.sleep(forTimeInterval: 0.05)
    }
    return check()
}
