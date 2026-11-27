// Finding a receiver when mDNS does not get through (PROTOCOL.md 1): a ladder that climbs by itself, as luchd's
// find/mod.rs does.
//
// 1. mDNS and, at the same time, a sweep of the LAN for the control port;
// 2. after a few seconds with nothing: Bluetooth LE, the receivers' cards over the air;
// 3. only when the person asked for it ("all"), as it needs them and opens the microphone and the camera: the TV
//    saying its card by sound, and its QR code held up to the camera.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum Rung: String, CaseIterable {
    case mdns, scan, ble, sound, qr

    /// What the person is told while it runs.
    public var trying: String {
        switch self {
        case .mdns: return "asking the network (mDNS)"
        case .scan: return "asking every address of this network"
        case .ble: return "listening for receivers over Bluetooth"
        case .sound: return "listening on the microphone: on the TV, open Luch and choose \"Say the address\""
        case .qr: return "looking through the camera: hold it up to the QR code on the TV"
        }
    }
}

/// Rungs that start together, and how long they are given before the next tier starts (sooner when all of them
/// have finished).
public struct FindTier {
    public var rungs: [Rung]
    public var patience: TimeInterval
}

public struct FindError: Error, LocalizedError {
    public let message: String
    public init(_ message: String) { self.message = message }
    public var errorDescription: String? { message }
}

/// The order of the climb and where it is.
public struct Ladder {
    /// mDNS answers within a second or two on a network that carries it; the sweep of a /24 takes about as long.
    public static let firstPatience: TimeInterval = 4
    public static let bleListen: TimeInterval = 12
    public static let personListen: TimeInterval = 30

    private var tiers: [FindTier]
    private var next = 0
    private var tierAt: TimeInterval = 0

    /// "auto" (what refresh climbs), "all" (also sound and the camera), or a comma-separated list of rungs to try
    /// right away.
    public init(mode: String) throws {
        let first = FindTier(rungs: [.mdns, .scan], patience: Ladder.firstPatience)
        let ble = FindTier(rungs: [.ble], patience: Ladder.bleListen)
        let person = FindTier(rungs: [.sound, .qr], patience: Ladder.personListen)
        switch mode {
        case "", "auto": tiers = [first, ble]
        case "all": tiers = [first, ble, person]
        default:
            let rungs = mode.split(separator: ",").map { Rung(rawValue: $0.trimmingCharacters(in: .whitespaces)) }
            guard !rungs.isEmpty, rungs.allSatisfy({ $0 != nil }) else {
                throw FindError("unknown way to look: \(mode) (auto, all, or some of mdns,scan,ble,sound,qr)")
            }
            tiers = [FindTier(rungs: rungs.compactMap { $0 }, patience: Ladder.personListen)]
        }
    }

    public var rungs: [Rung] { tiers.flatMap(\.rungs) }

    /// The rungs to start now: the first tier at once, the next one when nothing has been found and the last tier
    /// has had its time or has finished.
    public mutating func due(now: TimeInterval, foundAny: Bool, running: Int) -> [Rung] {
        guard next < tiers.count else { return [] }
        if next > 0 {
            let waited = max(0, now - tierAt)
            if foundAny || (running > 0 && waited < tiers[next - 1].patience) { return [] }
        }
        tierAt = now
        next += 1
        return tiers[next - 1].rungs
    }

    public var exhausted: Bool { next >= tiers.count }
}

/// What each rung is doing.
public struct FindProgress {
    public var running = false
    public var mode = ""
    public var rungs: [(rung: Rung, state: String, note: String)] = []
    public var found = 0
    public var elapsed: TimeInterval = 0

    public init() {}

    mutating func set(_ rung: Rung, _ state: String, _ note: String) {
        if let i = rungs.firstIndex(where: { $0.rung == rung }) { rungs[i] = (rung, state, note) }
    }

    /// The same JSON as luchd's FindProgress.
    public var json: [String: Any] {
        ["running": running, "mode": mode, "found": found, "elapsed": (elapsed * 10).rounded() / 10,
         "rungs": rungs.map { ["rung": $0.rung.rawValue, "state": $0.state, "note": $0.note] }]
    }
}

/// How the climb waits: the wall clock, or a test's.
public protocol FindClock: AnyObject {
    var now: TimeInterval { get }
    /// Waits up to `seconds`, or less when `wake` is signalled.
    func wait(_ seconds: TimeInterval, wake: DispatchSemaphore)
}

public final class WallClock: FindClock {
    public init() {}
    public var now: TimeInterval { ProcessInfo.processInfo.systemUptime }
    public func wait(_ seconds: TimeInterval, wake: DispatchSemaphore) { _ = wake.wait(timeout: .now() + seconds) }
}

/// Climbs `ladder`: starts the rungs as they fall due, follows them, and reports every change to `report`.
/// `found` says how many receivers are listed now; `run` starts one rung and calls back once with its note, or
/// throws its failure into the callback (on any thread).
public func climb(_ ladder: Ladder, mode: String, clock: FindClock, found: () -> Int,
                  run: (Rung, @escaping (Result<String, FindError>) -> Void) -> Void,
                  report: (FindProgress) -> Void) -> FindProgress {
    var ladder = ladder
    let t0 = clock.now
    var p = FindProgress()
    p.running = true
    p.mode = mode
    p.rungs = ladder.rungs.map { ($0, "waiting", "") }
    let lock = NSLock()
    let wake = DispatchSemaphore(value: 0)
    var finished: [(Rung, Result<String, FindError>)] = []
    var running = 0
    while true {
        lock.lock()
        let done = finished
        finished.removeAll()
        lock.unlock()
        for (r, result) in done {
            running -= 1
            switch result {
            case let .success(note): p.set(r, "done", note)
            case let .failure(e): p.set(r, "failed", e.message)
            }
            p.found = found()
            p.elapsed = clock.now - t0
            report(p)
        }
        p.found = found()
        p.elapsed = clock.now - t0
        let due = ladder.due(now: clock.now - t0, foundAny: p.found > 0, running: running)
        for r in due {
            p.set(r, "running", r.trying)
            running += 1
            run(r) { result in
                lock.lock()
                finished.append((r, result))
                lock.unlock()
                wake.signal()
            }
        }
        if !due.isEmpty { report(p) }
        if running == 0, p.found > 0 || ladder.exhausted { break }
        clock.wait(0.25, wake: wake)
    }
    for i in p.rungs.indices where p.rungs[i].state == "waiting" {
        p.rungs[i].state = "skipped"
        p.rungs[i].note = "a receiver was found before"
    }
    p.running = false
    p.found = found()
    p.elapsed = clock.now - t0
    report(p)
    return p
}
