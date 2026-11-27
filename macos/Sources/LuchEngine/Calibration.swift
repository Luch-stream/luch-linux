// A group's acoustic calibration (docs/research/sync-multiroom.md 6.7, PROTOCOL.md 4k): each member plays short
// sweeps at times on the group's pts clock, in turn; this Mac's microphone records them, and each member's correction
// is set so that all are heard together. It measures what no receiver can see of itself: a soundbar's or a projector's
// processing after its DAC. The arithmetic is luchd's (sync.rs), with its test vectors.
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreMedia
import Foundation
import LuchProto

/// What each member plays: `n` sweeps of `ms`, `gapMs` apart, at `gain` of full scale.
public struct ProbeSignal {
    public var n = 4
    public var ms = 200
    public var gapMs = 500
    public var gain = 0.3

    public static let standard = ProbeSignal()

    public init(n: Int = 4, ms: Int = 200, gapMs: Int = 500, gain: Double = 0.3) {
        (self.n, self.ms, self.gapMs, self.gain) = (n, ms, gapMs, gain)
    }

    /// One member's turn: its sweeps, then half a second of quiet before the next one sounds.
    public var slotUS: Int64 { Int64(n * (ms + gapMs)) * 1000 + 500_000 }

    /// When each of a member's sweeps is due to start, from its `at`.
    public func sweepStarts(at: Int64) -> [Int64] { (0..<n).map { at + Int64($0 * (ms + gapMs)) * 1000 } }

    /// The `probe` message for one member.
    public func message(id: String, at: Int64, gain: Double) -> [String: Any] {
        ["t": "probe", "id": id, "at": at, "signal": "sweep", "f0": Int(AcousticProbe.f0), "f1": Int(AcousticProbe.f1),
         "ms": ms, "n": n, "gap_ms": gapMs, "gain": gain]
    }
}

public enum Calibration {
    /// Each sweep is looked for this far around when its member says it reached the DAC.
    public static let windowMs = 150.0
    /// A member whose sweeps the detector refused is asked once more, louder: 0.6 in the design, but a receiver
    /// plays at most 0.5 of full scale.
    public static let retryGain = 0.5

    /// The first member's start, a pts of the audio stream: past D (a receiver has written the group's sound up to
    /// about now + D, and a sweep at a pts already written is `late`) and a second more for the message to arrive.
    public static func firstAt(nowUS: Int64, dMs: Double) -> Int64 {
        nowUS + Int64(((max(dMs, 0) + 1000) * 1000).rounded())
    }

    /// Member k's start, time-divided so that only one sounds at a time.
    public static func memberStarts(at: Int64, count: Int, signal: ProbeSignal) -> [Int64] {
        (0..<count).map { at + Int64($0) * signal.slotUS }
    }

    /// One member's calibration: when each sweep reached its DAC by its own reckoning (its offset included) and when
    /// the microphone heard it, both on the sender's clock (µs), nil where the detector did not trust an arrival.
    public struct Heard {
        public let id: String
        public let played: [Int64]
        public let heard: [Int64?]

        public init(id: String, played: [Int64], heard: [Int64?]) {
            (self.id, self.played, self.heard) = (id, played, heard)
        }

        /// The median of heard − played in ms, when every sweep was trusted and they agree within 0.5 ms.
        public var delayMs: Double? {
            guard !played.isEmpty, played.count == heard.count else { return nil }
            var d: [Double] = []
            for (p, a) in zip(played, heard) {
                guard let a else { return nil }
                d.append(Double(a - p) / 1000)
            }
            d.sort()
            let n = d.count
            let median = n % 2 == 1 ? d[n / 2] : (d[n / 2 - 1] + d[n / 2]) / 2
            return d[n - 1] - d[0] <= AcousticProbe.agreeMs ? median : nil
        }
    }

    /// Each member's new correction (ms, all made as late as the slowest), or nil for one not measured. It is
    /// absolute, the new offset itself: the one in use is inside `played`. The microphone's own capture latency is
    /// the same for all and cancels (luchd: offsets_from_heard).
    public static func offsets(fromHeard all: [Heard]) -> [(id: String, ms: Double?)] {
        let c = all.map(\.delayMs)
        let slowest = c.compactMap { $0 }.max()
        return zip(all, c).map { h, c in
            guard let c, let slowest else { return (h.id, nil) }
            return (h.id, ((slowest - c) * 10).rounded() / 10)
        }
    }
}

/// A microphone's recording: mono at the device's own rate, and the host-clock time of the first sample of each
/// buffer it came in.
public struct MicRecording {
    public var samples: [Float]
    public var rate: Double
    public var anchors: [(sample: Int, host: Double)]
    /// How long before its buffer's host time a sound reached the microphone (s): the device's own path, 31.5 ms on
    /// a MacBook's built-in array. Taken off every time heard, so "heard − played" is what the TV adds, plus the air.
    public var captureLatency: Double

    public init(samples: [Float], rate: Double, anchors: [(sample: Int, host: Double)], captureLatency: Double = 0) {
        (self.samples, self.rate, self.anchors, self.captureLatency) = (samples, rate, anchors, captureLatency)
    }

    /// sample = a + slope · (host − t0), fitted over all buffers: one buffer's time jitters, and the microphone's
    /// crystal is not the host's (100 ppm is 1 ms over 10 s). A fit far from the nominal rate is not trusted.
    private var line: (t0: Double, a: Double, slope: Double)? {
        guard let first = anchors.first else { return nil }
        let t0 = first.host
        let n = Double(anchors.count)
        let mx = anchors.reduce(0) { $0 + ($1.host - t0) } / n
        let my = anchors.reduce(0) { $0 + Double($1.sample) } / n
        var sxx = 0.0, sxy = 0.0
        for p in anchors {
            let dx = p.host - t0 - mx
            sxx += dx * dx
            sxy += dx * (Double(p.sample) - my)
        }
        var slope = sxx > 0 ? sxy / sxx : rate
        if abs(slope / rate - 1) > 0.005 { slope = rate }
        return (t0, my - slope * mx, slope)
    }

    /// Where in `samples` a sound that reached the microphone at a host-clock time lies (fractional).
    public func position(host: Double) -> Double? {
        line.map { $0.a + $0.slope * (host + captureLatency - $0.t0) }
    }

    /// When the sound at a (fractional) sample reached the microphone, on the host clock.
    public func host(position: Double) -> Double? {
        line.map { $0.t0 + (position - $0.a) / $0.slope - captureLatency }
    }

    /// The loudest sample: a muted or forbidden microphone records digital silence.
    public var peak: Float { samples.reduce(0) { max($0, abs($1)) } }
}

/// The microphone as the calibration uses it; the app's is AVAudioEngine's input, the tests' a synthesised room.
public protocol CalibrationMic: AnyObject {
    /// Opens the microphone (macOS asks the person the first time) and records until `stop`.
    func start() throws
    /// Everything recorded so far.
    func recording() -> MicRecording
    func stop()
}

public struct CalibrationError: Error, LocalizedError {
    public let errorDescription: String?
    public init(_ text: String) { errorDescription = text }
}

/// Runs one calibration of a playing group: the probes, the recording, the detector, the corrections.
public final class Calibrator {
    /// One member's outcome.
    public struct Outcome {
        public let id: String
        public let name: String
        public let measured: Bool
        /// heard − played (the median), ms: what the member adds after its DAC, plus the travel and the microphone's
        /// own latency, which is the same for all.
        public let delayMs: Double?
        /// The correction it plays with now: the new one when measured, else the one it had.
        public let offsetMs: Double
        /// The weakest sweep's peak over the noise before it, and how far apart the sweeps landed.
        public let psrDb: Double?
        public let spreadMs: Double?
        public let retried: Bool
        /// How the member knew when its sweeps reached the DAC: "timestamp", or "head" (a guess, trusted less).
        public let src: String?
        /// "ok", or why it was not measured.
        public let why: String

        public var json: [String: Any] {
            var j: [String: Any] = ["id": id, "name": name, "measured": measured, "offset_ms": offsetMs,
                                    "retried": retried, "why": why]
            if let delayMs { j["delay_ms"] = (delayMs * 100).rounded() / 100 }
            if let psrDb { j["psr_db"] = (psrDb * 10).rounded() / 10 }
            if let spreadMs { j["spread_ms"] = (spreadMs * 100).rounded() / 100 }
            if let src { j["src"] = src }
            return j
        }
    }

    /// A receiver answers once it played its last sweep; this long after its turn it counts as not answering.
    static let answerS = 3.0
    /// After the last sweep a recording runs on this long: the window, the sweep and its reverberation.
    static let tailUS: Int64 = 800_000

    private let group: GroupSession
    private let mic: CalibrationMic
    private let store: SyncStore
    private let signal: ProbeSignal
    private let inbox = Inbox()

    public init(group: GroupSession, mic: CalibrationMic, store: SyncStore = .shared, signal: ProbeSignal = .standard) {
        self.group = group
        self.mic = mic
        self.store = store
        self.signal = signal
    }

    /// How long it takes for `members` sounding receivers, in seconds: for the person waiting.
    public static func seconds(members: Int, signal: ProbeSignal = .standard) -> Double {
        Double(Int64(members) * signal.slotUS) / 1e6 + 1.5
    }

    /// Plays, records, measures and sets each sounding member's correction. Throws when it cannot start at all.
    public func run() throws -> [Outcome] {
        let members = group.members.filter(\.role.audio)
        guard members.count >= 2 else {
            throw CalibrationError("calibration needs two or more receivers playing sound in the group")
        }
        group.onMessage = { [inbox] id, msg in inbox.put(id, msg) }
        defer { group.onMessage = nil }
        try mic.start()
        defer { mic.stop() }

        var answers = pass(members, gain: signal.gain)
        var results = detect(answers)
        if let rec = results.recording, rec.peak == 0 {
            throw CalibrationError("the microphone hears nothing: is it muted, or not allowed (System Settings, " +
                                   "Privacy & Security, Microphone)?")
        }
        // Who played but was not heard well enough is asked once more, louder; who refused is not.
        let again = members.filter { results.measured[$0.receiver.id]?.accepted == false }
        if !again.isEmpty {
            let retry = pass(again, gain: Calibration.retryGain)
            for (id, a) in retry { answers[id] = a }
            let second = detect(retry)
            for (id, r) in second.measured { results.measured[id] = r }
        }
        return finish(members, answers: answers, results: results.measured, retried: Set(again.map(\.receiver.id)))
    }

    // MARK: - one pass

    private enum Answer {
        case played([Int64], src: String?)
        case refused(String)
    }

    private struct Detected {
        let result: AcousticProbe.Result
        let heard: [Int64?]
        var accepted: Bool { result.accepted }
    }

    private func nowUS() -> Int64 {
        Int64(((CMTimeGetSeconds(CMClockGetTime(CMClockGetHostTimeClock())) - group.clockZero) * 1e6).rounded())
    }

    /// Sends each member its probe, its turn after the one before, and waits for every answer (or its time).
    private func pass(_ members: [GroupMember], gain: Double) -> [String: Answer] {
        let probe = UUID().uuidString
        let now = nowUS()
        let d = group.delay
        let starts = Calibration.memberStarts(at: Calibration.firstAt(nowUS: now, dMs: max(d.video, d.audio)),
                                              count: members.count, signal: signal)
        var answers: [String: Answer] = [:]
        for (m, at) in zip(members, starts) where !group.send(signal.message(id: probe, at: at, gain: gain),
                                                              to: m.receiver.id) {
            answers[m.receiver.id] = .refused("not connected")
        }
        let waiting = Set(members.map(\.receiver.id)).subtracting(answers.keys)
        // Each sweep is heard at its pts + D + the member's offset; probe_done comes after its last one.
        let end = (starts.last ?? now) + signal.slotUS + Int64(max(d.video, d.audio) * 1000)
        let got = inbox.wait(probe: probe, from: waiting, until: Date() + Double(end - now) / 1e6 + Self.answerS)
        for id in waiting {
            answers[id] = got[id].map(Self.answer) ?? .refused("no answer to the probe")
        }
        // The recording must reach past the last sweep heard, wherever its member says it played.
        let last = answers.values.compactMap { a -> Int64? in
            if case .played(let p, _) = a { return p.max() }
            return nil
        }.max() ?? end
        let wait = Double(last + Self.tailUS - nowUS()) / 1e6
        if wait > 0 { Thread.sleep(forTimeInterval: wait) }
        return answers
    }

    private static func answer(_ msg: [String: Any]) -> Answer {
        if msg["t"] as? String == "probe_done" {
            if msg["dry_run"] as? Bool == true { return .refused("it only answered a dry run: update the receiver") }
            let played = (msg["played"] as? [Any] ?? []).compactMap { ($0 as? NSNumber)?.int64Value }
            return played.isEmpty ? .refused("it played nothing") : .played(played, src: msg["src"] as? String)
        }
        switch msg["code"] as? String ?? "" {
        case "probe_off": return .refused("it plays no sound here, or calibration is off in its settings")
        case "not_playing": return .refused("it is not playing this group's sound")
        case "not_synced": return .refused("its clock is not in step yet: try again in a few seconds")
        case "late": return .refused("the probe came too late for its sound: try again")
        case let code: return .refused(msg["detail"] as? String ?? (code.isEmpty ? "refused" : code))
        }
    }

    /// The detector over the recording so far, for every member that played.
    private func detect(_ answers: [String: Answer]) -> (recording: MicRecording?, measured: [String: Detected]) {
        let rec = mic.recording()
        let zero = group.clockZero
        let probe = AcousticProbe(rate: Int(rec.rate.rounded()))
        var out: [String: Detected] = [:]
        for (id, a) in answers {
            guard case .played(let played, _) = a else { continue }
            let positions = played.compactMap { rec.position(host: zero + Double($0) / 1e6) }
            guard positions.count == played.count else {
                let none = AcousticProbe.Result(accepted: false, delaySamples: .nan, arrivals: [],
                                                why: "the microphone gave no timestamps")
                out[id] = Detected(result: none, heard: played.map { _ in nil })
                continue
            }
            let r = probe.measure(rec.samples, scheduled: positions, windowMs: Calibration.windowMs)
            let heard = played.indices.map { i -> Int64? in
                guard i < r.arrivals.count, r.arrivals[i].trusted, let t = rec.host(position: r.arrivals[i].at) else {
                    return nil
                }
                return Int64(((t - zero) * 1e6).rounded())
            }
            out[id] = Detected(result: r, heard: heard)
            notes.append(Self.note(id: id, played: played, positions: positions, result: r, rec: rec, probe: probe))
        }
        lastRecording = rec
        return (rec, out)
    }

    // MARK: - what was heard, kept for a look afterwards

    private var notes: [[String: Any]] = []
    private var lastRecording: MicRecording?

    /// One member in one pass: where its sweeps were expected and found, and a wider look (100 ms before to 450 ms
    /// after) that shows where a sweep the window missed really was.
    private static func note(id: String, played: [Int64], positions: [Double], result r: AcousticProbe.Result,
                             rec: MicRecording, probe: AcousticProbe) -> [String: Any] {
        let ms = { (samples: Double) in (samples * 1000 / rec.rate * 100).rounded() / 100 }
        let wide = positions.map { p -> [String: Any] in
            let a = probe.find(rec.samples, from: Int(p) - Int(rec.rate * 0.1), to: Int(p) + Int(rec.rate * 0.45))
            return ["delay_ms": a.at.isNaN ? NSNull() : ms(a.at - p), "psr_db": (a.psrDb * 10).rounded() / 10,
                    "trusted": a.trusted, "why": a.why]
        }
        let found = zip(r.arrivals, positions).map { a, p -> [String: Any] in
            ["delay_ms": a.at.isNaN ? NSNull() : ms(a.at - p), "psr_db": (a.psrDb * 10).rounded() / 10,
             "trusted": a.trusted, "why": a.why]
        }
        return ["id": id, "played_us": played, "position": positions, "found": found, "wide": wide, "why": r.why]
    }

    /// Writes the last recording (16-bit WAV) and what each pass expected and found (JSON) into `directory`.
    public func save(to directory: URL) throws {
        guard let rec = lastRecording else { return }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try Self.wav(rec.samples, rate: Int(rec.rate.rounded())).write(to: directory.appendingPathComponent("mic.wav"))
        let meta: [String: Any] = ["rate": rec.rate, "clock_zero": group.clockZero, "passes": notes]
        try JSONSerialization.data(withJSONObject: meta, options: [.prettyPrinted, .sortedKeys])
            .write(to: directory.appendingPathComponent("heard.json"))
    }

    static func wav(_ samples: [Float], rate: Int) -> Data {
        var d = Data()
        func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
        func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
        let bytes = UInt32(samples.count * 2)
        d.append(contentsOf: Array("RIFF".utf8)); u32(36 + bytes); d.append(contentsOf: Array("WAVE".utf8))
        d.append(contentsOf: Array("fmt ".utf8)); u32(16); u16(1); u16(1); u32(UInt32(rate)); u32(UInt32(rate * 2))
        u16(2); u16(16)
        d.append(contentsOf: Array("data".utf8)); u32(bytes)
        for s in samples { u16(UInt16(bitPattern: Int16(max(-1, min(1, s)) * 32767))) }
        return d
    }

    // MARK: - the corrections

    private func finish(_ members: [GroupMember], answers: [String: Answer], results: [String: Detected],
                        retried: Set<String>) -> [Outcome] {
        var heard: [Calibration.Heard] = []
        for m in members {
            let id = m.receiver.id
            if case .played(let played, _) = answers[id], let r = results[id] {
                heard.append(Calibration.Heard(id: id, played: played, heard: r.heard))
            }
        }
        let offsets = Dictionary(Calibration.offsets(fromHeard: heard).map { ($0.id, $0.ms) }) { a, _ in a }
        return members.map { m in
            let id = m.receiver.id
            let h = heard.first { $0.id == id }
            let r = results[id]
            var src: String?
            if case .played(_, let s)? = answers[id] { src = s }
            let psr = r?.result.arrivals.map(\.psrDb).min()
            let spread: Double? = h.flatMap { h -> Double? in
                let d = zip(h.played, h.heard).compactMap { p, a in a.map { Double($0 - p) / 1000 } }
                return d.count == h.played.count ? (d.max() ?? 0) - (d.min() ?? 0) : nil
            }
            if let ms = offsets[id] ?? nil {
                store.setOffset(ms, key: Sync.offsetKey(id, route: store.route(id)))
                group.setOffset(ms, for: id)
                return Outcome(id: id, name: m.receiver.name, measured: true, delayMs: h?.delayMs, offsetMs: ms,
                               psrDb: psr, spreadMs: spread, retried: retried.contains(id), src: src, why: "ok")
            }
            let why: String
            switch answers[id] {
            case .refused(let text)?: why = text
            default: why = r.map { $0.accepted ? "the sweeps disagree" : $0.result.why } ?? "not heard"
            }
            return Outcome(id: id, name: m.receiver.name, measured: false, delayMs: nil,
                           offsetMs: store.currentOffset(id), psrDb: psr, spreadMs: spread,
                           retried: retried.contains(id), src: src, why: why)
        }
    }
}

/// The members' answers to a probe, as they come on their control channels.
private final class Inbox {
    private let cond = NSCondition()
    private var got: [String: [[String: Any]]] = [:]

    func put(_ id: String, _ msg: [String: Any]) {
        guard ["probe_done", "error"].contains(msg["t"] as? String ?? "") else { return }
        cond.lock()
        got[id, default: []].append(msg)
        cond.broadcast()
        cond.unlock()
    }

    /// Each member's answer to `probe` (its probe_done, or an error about probing), once all are in or at `until`.
    func wait(probe: String, from ids: Set<String>, until: Date) -> [String: [String: Any]] {
        cond.lock()
        defer { cond.unlock() }
        while true {
            var found: [String: [String: Any]] = [:]
            for id in ids {
                found[id] = got[id]?.first { m in
                    if m["t"] as? String == "probe_done" { return m["id"] as? String == probe }
                    // An error about probing; one without an id is about a probe it could not read.
                    return m["for"] as? String == "probe" && (m["id"] as? String).map { $0 == probe } ?? true
                }
            }
            if found.count == ids.count || !cond.wait(until: until) {
                for id in found.keys { got[id] = nil }
                return found
            }
        }
    }
}
