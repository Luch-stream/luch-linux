// The discovery ladder on a test clock, as luchd's find/mod.rs tests climb it.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
@testable import LuchFind
import LuchLocal
import XCTest

/// Time that moves only when the climb waits; rungs finish at the times they are given.
final class TestClock: FindClock {
    private(set) var now: TimeInterval = 0
    private var timers: [(at: TimeInterval, run: () -> Void)] = []

    func after(_ seconds: TimeInterval, _ run: @escaping () -> Void) { timers.append((now + seconds, run)) }

    func wait(_ seconds: TimeInterval, wake: DispatchSemaphore) {
        now += seconds
        let due = timers.filter { $0.at <= now + 1e-9 }.sorted { $0.at < $1.at }
        timers.removeAll { $0.at <= now + 1e-9 }
        due.forEach { $0.run() }
    }
}

final class LadderTests: XCTestCase {
    func testTheLadderClimbsOnlyWhileNothingIsFound() throws {
        var l = try Ladder(mode: "auto")
        XCTAssertEqual(l.due(now: 0, foundAny: false, running: 0), [.mdns, .scan])
        XCTAssertEqual(l.due(now: 1, foundAny: false, running: 2), [])
        XCTAssertEqual(l.due(now: 3, foundAny: false, running: 1), [], "mDNS still has time")
        XCTAssertEqual(l.due(now: 4, foundAny: false, running: 1), [.ble])
        XCTAssertTrue(l.exhausted)

        // Found by the sweep in the first second: Bluetooth is never tried.
        l = try Ladder(mode: "auto")
        _ = l.due(now: 0, foundAny: false, running: 0)
        XCTAssertEqual(l.due(now: 10, foundAny: true, running: 0), [])

        // Both first rungs gave up early: the next tier starts at once.
        l = try Ladder(mode: "all")
        _ = l.due(now: 0, foundAny: false, running: 0)
        XCTAssertEqual(l.due(now: 2, foundAny: false, running: 0), [.ble])
        XCTAssertEqual(l.due(now: 2.1, foundAny: false, running: 0), [.sound, .qr])

        var some = try Ladder(mode: "qr, ble")
        XCTAssertEqual(some.due(now: 0, foundAny: false, running: 0), [.qr, .ble])
        XCTAssertThrowsError(try Ladder(mode: "smoke signals"))
    }

    /// The whole climb with fake rungs: nothing on the LAN, Bluetooth hears the TV after 3 s.
    func testAClimbSaysWhatItTriesAndStopsWhenFound() throws {
        let clock = TestClock()
        var found = 0
        var started: [(Rung, TimeInterval)] = []
        var reports: [[String: Any]] = []
        let p = climb(try Ladder(mode: "all"), mode: "all", clock: clock, found: { found }, run: { r, done in
            started.append((r, clock.now))
            switch r {
            // Longer than its tier's patience: Bluetooth starts at 4 s without waiting for it.
            case .mdns: clock.after(10) { done(.success("0 found")) }
            case .scan: clock.after(2) { done(.success("asked 253 addresses on en0: no receiver")) }
            case .ble: clock.after(3) { found = 1; done(.success("Living room TV")) }
            default: XCTFail("sound and camera are not climbed to once a receiver is found")
            }
        }, report: { reports.append($0.json) })
        XCTAssertEqual(started.map(\.0), [.mdns, .scan, .ble])
        XCTAssertEqual(started.map(\.1), [0, 0, 4])
        XCTAssertEqual(p.found, 1)
        XCTAssertFalse(p.running)
        let j = p.json
        let rungs = j["rungs"] as! [[String: Any]]
        XCTAssertEqual(rungs[2]["state"] as? String, "done")
        XCTAssertEqual(rungs[2]["note"] as? String, "Living room TV")
        XCTAssertEqual(rungs[3]["state"] as? String, "skipped")
        XCTAssertEqual(rungs[4]["state"] as? String, "skipped")
        // Found at 7 s; the climb ends when mDNS, still running, has finished.
        XCTAssertEqual(j["elapsed"] as! Double, 10, accuracy: 0.6)
        // The person was told what was tried, in order.
        let first = (reports.first?["rungs"] as? [[String: Any]])?[1]
        XCTAssertEqual(first?["note"] as? String, Rung.scan.trying)

        // luchctl's lines for it: what started and ended, and the count at the end.
        var lines: [String] = []
        var last: [String: Any] = ["running": true]
        for r in reports {
            lines += FindText.progressLines(before: last, now: r)
            last = r
        }
        XCTAssertEqual(lines.first, " mdns trying: asking the network (mDNS)")
        XCTAssertTrue(lines.contains("  ble done: Living room TV"), "\(lines)")
        XCTAssertEqual(lines.last, "1 receiver found")
    }

    /// A failed rung is said as such, and the next one starts at once.
    func testARungThatCannotRunHandsOverAtOnce() throws {
        let clock = TestClock()
        var starts: [(Rung, TimeInterval)] = []
        let p = climb(try Ladder(mode: "all"), mode: "all", clock: clock, found: { 0 }, run: { r, done in
            starts.append((r, clock.now))
            switch r {
            case .ble: done(.failure(FindError("Bluetooth is off")))
            case .sound, .qr: clock.after(30) { done(.success("heard nothing")) }
            default: done(.success("nothing"))
            }
        }, report: { _ in })
        XCTAssertEqual(starts.map(\.0), Rung.allCases)
        // Within a tick or two of each other, not 4 and 16 s apart.
        XCTAssertTrue(starts.allSatisfy { $0.1 <= 0.5 }, "\(starts)")
        let rungs = p.json["rungs"] as! [[String: Any]]
        XCTAssertEqual(rungs[2]["state"] as? String, "failed")
        XCTAssertEqual(rungs[2]["note"] as? String, "Bluetooth is off")
        XCTAssertEqual(p.found, 0)
        XCTAssertEqual(FindText.progressLines(before: ["running": true], now: p.json).last,
                       "nothing found; `luchctl add <address>` takes a receiver by hand")
    }
}
