// Several receivers in step (PROTOCOL.md 4k): one capture and one encode, sent to every member on its own sockets
// with the playout delay D in each packet; each member gets the streams of its role, its clock requests are answered,
// and D follows what the members report, chosen by SyncPolicy as luchd chooses it.
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreMedia
import Foundation
import LuchMedia
import LuchNet
import LuchProto

/// A receiver of a group and what it presents.
public struct GroupMember {
    public var receiver: Receiver
    public var role: GroupRole

    public init(_ receiver: Receiver, role: GroupRole) {
        self.receiver = receiver
        self.role = role
    }
}

public final class GroupSession {
    /// What the group is about: the picture's latency (interactive) or one delay for all (watching).
    public let mode: SyncMode
    /// The Mac's sound only, every member a speaker; else the screen and its sound.
    public let soundOnly: Bool
    public let id = UUID().uuidString
    /// Each member's receiver id, in the order given (the first shows the picture unless sound only).
    public var receiverIDs: [String] { parts.map(\.receiver.id) }
    public var onStats: (([String: Any]) -> Void)?
    /// The session ended on its own (the first member stopped, capture failed).
    public var onStopped: ((String) -> Void)?
    /// A member's person is asked to Allow this Mac (PROTOCOL.md 3a).
    public var onWaitingForAllow: ((String) -> Void)?

    /// Picture tolerance and the watching floor, as luchd's.
    static let toleranceMs = 20.0
    static let floorMs = 150.0

    private let options: MirrorOptions
    private let store: SyncStore
    private let timeline = UInt32.random(in: 0...UInt32.max)
    private var parts: [Part]
    private let lock = NSLock()
    private var policy: SyncPolicy
    private var started = Date()
    private var d = (video: 0.0, audio: 0.0)
    private var pending: [(Int, Double)] = []
    private var last: [String: Any] = [:]
    private var running = false
    private var stopping = false

    private var plan: CodecPlan?
    private var encoder: VideoEncoder?
    private var audio: OpusAudio?
    private var capture: ScreenCapture?
    private var timers: [DispatchSourceTimer] = []
    private var clocks: [ClockResponder] = []
    private let wantIDR = Flag()
    private let needHead = Flag()
    private var lastAU: (data: Data, flags: PacketFlags, hostLat: UInt16)?
    private var lastSentAt = Date.distantPast
    /// The pts clock's zero: every pts and every clock reply of the session counts from it, on the host clock.
    private var base = CMClockGetTime(CMClockGetHostTimeClock())
    private var lastFrames: UInt64 = 0
    private var lastBytes: UInt64 = 0
    private var lastLatency: UInt64 = 0

    /// One member: its connection, its sockets and what it reported.
    private final class Part {
        let receiver: Receiver
        let role: GroupRole
        var control: ControlChannel?
        var caps: ReceiverCaps?
        var video: UdpSender?
        var audio: UdpSender?
        var report = SyncReport()
        var stats: [String: Any] = [:]
        var present = true
        /// The correction in use and the route it was chosen for.
        var offset: (ms: Double, route: String?)

        init(_ m: GroupMember, offset: Double) {
            receiver = m.receiver
            role = m.role
            self.offset = (offset, nil)
        }
    }

    public init(members: [GroupMember], mode: SyncMode, soundOnly: Bool, options: MirrorOptions,
                store: SyncStore = .shared) {
        self.mode = mode
        self.soundOnly = soundOnly
        self.options = options
        self.store = store
        // Sound only: every member is a speaker, whatever was asked.
        parts = members.map { Part(soundOnly ? GroupMember($0.receiver, role: .audio) : $0,
                                   offset: store.currentOffset($0.receiver.id)) }
        policy = SyncPolicy(mode: mode, toleranceMs: Self.toleranceMs, floorMs: Self.floorMs)
    }

    // MARK: - lifecycle

    /// Connects to every member, starts each with its part of the group, then capture and the streams. Blocks until
    /// all are playing or one refused (then none plays).
    public func start() throws {
        guard parts.count >= 2 else { throw EngineError.refused("two or more receivers, one role each") }
        guard Set(parts.map(\.receiver.id)).count == parts.count else {
            throw EngineError.refused("a receiver is in the group twice")
        }
        if !soundOnly && !parts[0].role.video {
            throw EngineError.refused("the first receiver shows the picture: role video or both")
        }
        do {
            try connectAll()
            try startAll()
        } catch {
            for p in parts {
                p.control?.send(["t": "stop"])
                p.control?.bye()
            }
            throw error
        }
        running = true
        startTimers()
        do {
            try startMedia()
        } catch {
            stop(reason: "capture: \(error.localizedDescription)")
            throw error
        }
        log("group: \(parts.map { "\($0.receiver.name) (\($0.role.rawValue))" }.joined(separator: ", ")), " +
            "\(mode.wire)\(soundOnly ? ", sound only" : "")")
    }

    public func stop(reason: String) {
        lock.lock()
        guard running, !stopping else { lock.unlock(); return }
        stopping = true
        lock.unlock()
        log("group: finishing: \(reason)")
        for p in parts {
            p.control?.send(["t": "stop"])
            p.control?.bye()
        }
        timers.forEach { $0.cancel() }
        timers = []
        let done = DispatchSemaphore(value: 0)
        let capture = self.capture
        Task {
            await capture?.stop()
            done.signal()
        }
        _ = done.wait(timeout: .now() + 2)
        clocks.forEach { $0.stop() }
        lock.lock()
        running = false
        lock.unlock()
        onStopped?(reason)
    }

    /// A member's own correction, at once (`luchctl offset`, the slider); false when it is not in this group.
    @discardableResult
    public func setOffset(_ ms: Double, for receiverID: String) -> Bool {
        lock.lock()
        guard let p = parts.first(where: { $0.receiver.id == receiverID && $0.present }) else {
            lock.unlock()
            return false
        }
        p.offset.ms = ms
        lock.unlock()
        p.control?.send(["t": "offset", "ms": ms])
        return true
    }

    /// Sends a control message to one member (the latency lab's flag); false when it is not in this group.
    @discardableResult
    public func send(_ message: [String: Any], to receiverID: String) -> Bool {
        guard let p = parts.first(where: { $0.receiver.id == receiverID }), let c = p.control else { return false }
        return c.send(message)
    }

    /// The last second's numbers in luchd's shape: `sender`, `receiver` (the first member's), `est_ms` and `group`
    /// (`d_video_ms`, `d_audio_ms`, `members`).
    public var stats: [String: Any] {
        lock.lock()
        defer { lock.unlock() }
        return last
    }

    /// D now, in ms: the picture's and the sound's.
    public var delay: (video: Double, audio: Double) {
        lock.lock()
        defer { lock.unlock() }
        return d
    }

    // MARK: - start

    private func connectAll() throws {
        let identity = senderIdentity()
        for (k, p) in parts.enumerated() {
            let r = p.receiver
            // Each member over TLS and paired when it offers that (Connector), asked for its code as any connection is.
            let c = try Connector.shared.open(r, hello: ["t": "hello", "v": Int(Proto.version), "id": identity.id,
                                                         "name": identity.name, "app": Proto.app,
                                                         "want": ["mirror", "reports"]])
            let ctl = c.channel
            p.control = ctl
            ctl.onEvent = { [weak self] msg in self?.handle(k, msg) }
            ctl.onClosed = { [weak self] why in self?.handle(k, ["t": "_closed", "why": why]) }
            let welcome = c.welcome
            guard let caps = ReceiverCaps(welcome: welcome) else { throw EngineError.noWelcome }
            p.caps = caps
            guard Sync.capable(welcome) else { throw EngineError.refused("\(r.name) cannot play in step yet: update it") }
            if p.role.audio && (soundOnly || k > 0) && !caps.hasOpus {
                throw EngineError.refused("\(r.name) cannot play this Mac's sound (no Opus)")
            }
            if soundOnly && !caps.modes.contains("audio") {
                throw EngineError.refused("\(r.name) cannot play sound only yet: update it")
            }
        }
    }

    /// The codec every picture member decodes: the first member's plan, or H.264 when another cannot decode it.
    private func planVideo() -> CodecPlan? {
        guard !soundOnly, let caps = parts[0].caps else { return nil }
        var o = options
        var plan = CodecPlanner.plan(caps: caps, receiverID: parts[0].receiver.id, options: o)
        if parts.contains(where: { $0.role.video && $0.caps?.canDecode(plan.codec) == false }) {
            o.codec = .h264
            plan = CodecPlanner.plan(caps: caps, receiverID: parts[0].receiver.id, options: o)
        }
        return plan
    }

    private func startAll() throws {
        plan = planVideo()
        let video: [String: Any]? = plan.map {
            ["codec": $0.codec.rawValue, "width": $0.width, "height": $0.height, "fps": $0.fps]
        }
        for (k, p) in parts.enumerated() {
            guard let ctl = p.control, let caps = p.caps else { continue }
            var start: [String: Any] = ["t": "start", "mode": "audio"]
            if p.role.video, let video {
                start = ["t": "start", "mode": "mirror", "video": video]
            }
            // The first member's sound is its own choice (luchd: the mirroring target's); the others' plays as it comes.
            let audioMode = k == 0 && !soundOnly ? store.audioMode(p.receiver.id) : .on
            let wantsSound = soundOnly || (options.audio && caps.hasOpus)
            let audioBlock = wantsSound ? audioMode.startBlock(limits: caps.limits, frameMs: options.audioFrameMs) : nil
            if p.role.audio, let audioBlock { start["audio"] = audioBlock }
            start["group"] = Sync.groupBlock(id: id, timeline: timeline, role: p.role, mode: mode,
                                             toleranceMs: Self.toleranceMs, offsetMs: p.offset.ms)
            let ready = try startWhenAllowed(ctl, start, name: p.receiver.name)
            let r = p.receiver
            // Each member's media sealed with the key of its own ready (PROTOCOL.md 4).
            let seal = try MediaSeal.key(ready: ready).flatMap { MediaSeal(key: $0) }
            if start["video"] != nil, let port = (ready["video_port"] as? NSNumber)?.uint16Value, port > 0 {
                p.video = UdpSender.open(kind: .video, ip: r.address, port: port, interfaceIndex: r.interfaceIndex,
                                         dscp: 40, seal: seal)
                if p.video == nil { throw EngineError.socket }
            }
            if start["audio"] != nil, let port = (ready["audio_port"] as? NSNumber)?.uint16Value, port > 0 {
                p.audio = UdpSender.open(kind: .audio, ip: r.address, port: port, interfaceIndex: r.interfaceIndex,
                                         dscp: 48, seal: seal)
                if p.audio == nil { throw EngineError.socket }
            }
            log("group: \(r.name) ready (\(p.role.rawValue))")
        }
        base = CMClockGetTime(CMClockGetHostTimeClock())
        started = Date()
        let base = self.base
        // The pts clock: the host clock from the session's zero, the clock every pts here is on.
        let now: () -> UInt64? = {
            let us = (CMTimeGetSeconds(CMClockGetTime(CMClockGetHostTimeClock())) - CMTimeGetSeconds(base)) * 1e6
            return UInt64(max(0, us))
        }
        for p in parts {
            for tx in [p.video, p.audio].compactMap({ $0 }) {
                tx.startTimeline()
                clocks.append(ClockResponder(on: tx, now: now))
            }
        }
    }

    /// As Engine's: the start again every 1.5 s while the receiver's person is asked, for a minute.
    private func startWhenAllowed(_ ctl: ControlChannel, _ start: [String: Any], name: String) throws -> [String: Any] {
        let until = Date().addingTimeInterval(60)
        var told = false
        while Date() < until {
            ctl.send(start)
            guard let r = ctl.waitForAny(["ready", "error"], timeout: 5) else {
                throw EngineError.refused("\(name) did not answer start")
            }
            if r["t"] as? String == "ready" { return r }
            let detail = r["detail"] as? String ?? r["code"] as? String ?? "refused"
            guard r["code"] as? String == "not_allowed", !detail.contains("refused") else {
                throw EngineError.refused("\(name): \(detail)")
            }
            if !told {
                told = true
                onWaitingForAllow?(name)
            }
            Thread.sleep(forTimeInterval: 1.5)
            if ctl.isClosed { throw EngineError.refused("refused on \(name)") }
        }
        throw EngineError.refused("nobody pressed Allow on \(name)")
    }

    // MARK: - media: one capture, one encode, a send per member

    private func live<T>(_ kind: KeyPath<Part, T?>) -> [T] {
        lock.lock()
        defer { lock.unlock() }
        return parts.filter(\.present).compactMap { $0[keyPath: kind] }
    }

    private func startMedia() throws {
        let videos = live(\.video)
        if !videos.isEmpty, let plan {
            guard let encoder = VideoEncoder(codec: plan.codec, width: plan.width, height: plan.height, fps: plan.fps,
                                             kbps: options.bitrate, profile: options.profile) else {
                throw EngineError.encoder
            }
            self.encoder = encoder
            encoder.onPacket = { [weak self] data, idr, pts in
                guard let self else { return }
                if idr { self.needHead.set() }
                let flags: PacketFlags = idr ? .idr : []
                let hostLat = self.hostLatency(pts)
                let bytes = [UInt8](data)
                for tx in self.live(\.video) {
                    tx.send(frame: bytes, flags: flags, ptsUS: self.relativeUS(pts), hostLat: hostLat)
                }
                self.lastAU = (data, flags, hostLat)
                self.lastSentAt = Date()
            }
        }
        if !live(\.audio).isEmpty {
            guard let audio = OpusAudio(frameMs: options.audioFrameMs) else { throw EngineError.socket }
            self.audio = audio
            needHead.set()
            audio.onPacket = { [weak self] data, pts in
                guard let self else { return }
                let head = self.needHead.take()
                let bytes = [UInt8](data)
                for tx in self.live(\.audio) {
                    if head { tx.send(frame: [UInt8](audio.head), flags: .config, ptsUS: self.relativeUS(pts), hostLat: 0) }
                    tx.send(frame: bytes, flags: [], ptsUS: self.relativeUS(pts), hostLat: self.hostLatency(pts))
                }
            }
        }
        if options.synthetic {
            startSynthetic()
            return
        }
        let capture = ScreenCapture()
        self.capture = capture
        capture.onFrame = { [weak self] buffer, pts in
            guard let self, let encoder = self.encoder else { return }
            WarmFilter.apply(buffer, strength: self.options.warmth)
            encoder.encode(buffer, pts: pts, forceIDR: self.wantIDR.take())
        }
        capture.onAudio = { [weak self] sample in self?.audio?.push(sample) }
        capture.onStop = { [weak self] why in self?.stop(reason: "capture: \(why)") }
        let started = DispatchSemaphore(value: 0)
        var failure: Error?
        // ScreenCaptureKit hands out sound only from a screen stream; for sound only it is a tiny one, unused.
        let size = plan.map { (w: $0.width, h: $0.height, fps: $0.fps) } ?? (w: 64, h: 36, fps: 1)
        let withAudio = audio != nil
        Task {
            do {
                try await capture.start(displayIndex: options.display, width: size.w, height: size.h, fps: size.fps,
                                        audio: withAudio, fit: options.fit)
            } catch {
                failure = error
            }
            started.signal()
        }
        started.wait()
        if let failure { throw failure }
    }

    /// Test frames or silence at the real pace, without capture: the whole path runs and nothing is seen or heard.
    private func startSynthetic() {
        let queue = DispatchQueue(label: "luch.group.synthetic", qos: .userInteractive)
        if let audio {
            let ms = options.audioFrameMs
            let silence = [Float](repeating: 0, count: 48 * ms * 2)
            timers.append(repeating(.milliseconds(ms), on: queue) {
                audio.emit(silence, pts: CMClockGetTime(CMClockGetHostTimeClock()))
            })
        }
        if let encoder, let plan {
            var index = 0
            timers.append(repeating(.nanoseconds(1_000_000_000 / plan.fps), on: queue) { [weak self] in
                guard let self, let frame = SyntheticFrame.make(width: plan.width, height: plan.height, index: index) else {
                    return
                }
                index += 1
                encoder.encode(frame, pts: CMClockGetTime(CMClockGetHostTimeClock()), forceIDR: self.wantIDR.take())
            })
        }
    }

    private func repeating(_ every: DispatchTimeInterval, on queue: DispatchQueue,
                           _ run: @escaping () -> Void) -> DispatchSourceTimer {
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now(), repeating: every)
        t.setEventHandler(handler: run)
        t.resume()
        return t
    }

    // MARK: - events

    private func handle(_ k: Int, _ msg: [String: Any]) {
        switch msg["t"] as? String ?? "" {
        case "keyframe":
            wantIDR.set()
        case "stats":
            report(k, msg)
        case "media_key":
            // The TV's remote, for the player on this Mac (PROTOCOL.md 4d).
            MediaKeys.post(msg["key"] as? String ?? "")
        case "stop", "bye", "_closed":
            lock.lock()
            let wasRunning = running && !stopping
            let present = parts[k].present
            parts[k].present = false
            lock.unlock()
            guard wasRunning, present else { return }
            // The first member is the session; another one leaving leaves the others playing.
            if k == 0 {
                stop(reason: "\(parts[0].receiver.name) stopped")
            } else {
                log("group: \(parts[k].receiver.name) left (\(msg["t"] as? String ?? ""))")
            }
        default:
            break
        }
    }

    /// A member's stats: what the policy needs, and a new output route's own correction (a soundbar turned on).
    private func report(_ k: Int, _ stats: [String: Any]) {
        lock.lock()
        defer { lock.unlock() }
        let p = parts[k]
        p.stats = stats
        p.report = SyncReport(stats: stats)
        guard let route = p.report.route, p.offset.route != route else { return }
        if store.route(p.receiver.id) != route { store.rememberRoute(route, for: p.receiver.id) }
        let ms = store.offset(Sync.offsetKey(p.receiver.id, route: route))
            ?? Sync.routePresetMs(route, audioMs: p.report.audioMs)
        p.offset = (ms, route)
        pending.append((k, ms))
    }

    // MARK: - once a second

    private func startTimers() {
        timers.append(repeating(.seconds(1), on: DispatchQueue(label: "luch.group.stats")) { [weak self] in self?.tick() })
        timers.append(repeating(.milliseconds(100), on: DispatchQueue(label: "luch.group.keepalive")) { [weak self] in
            self?.keepalive()
        })
    }

    /// D from what the members reported, into every packet from now on; the numbers to every member and to the app.
    private func tick() {
        lock.lock()
        let video = parts.filter { $0.present && $0.role.video }.compactMap(\.report.videoPtsMs)
        let need = parts.filter { $0.present && (mode == .watching || $0.role.audio) }.compactMap(\.report.needMs)
        d = policy.update(nowS: Date().timeIntervalSince(started), videoPtsMs: video, needMs: need)
        let (dv, da) = (Sync.headerUnits(d.video), Sync.headerUnits(d.audio))
        let offsets = pending
        pending = []
        let members: [[String: Any]] = parts.map { p in
            ["id": p.receiver.id, "name": p.receiver.name, "role": p.role.rawValue, "present": p.present,
             "need_ms": p.report.needMs ?? NSNull(), "video_pts_ms": p.report.videoPtsMs ?? NSNull(),
             "route": p.report.route ?? NSNull(), "offset_ms": p.offset.ms]
        }
        let group: [String: Any] = ["d_video_ms": d.video, "d_audio_ms": d.audio, "members": members]
        let primary = parts[0].stats
        let live = parts.filter(\.present)
        lock.unlock()

        for p in live {
            p.video?.setDelay(dv)
            p.audio?.setDelay(da)
        }
        for (k, ms) in offsets { parts[k].control?.send(["t": "offset", "ms": ms]) }

        let first = live.compactMap { $0.video ?? $0.audio }.first
        let snap = first?.snapshot()
        let (frames, bytes, latency) = (snap?.frames ?? 0, snap?.bytes ?? 0, snap?.latencySum ?? 0)
        let df = frames >= lastFrames ? frames - lastFrames : frames
        let db = bytes >= lastBytes ? bytes - lastBytes : bytes
        let dl = latency >= lastLatency ? latency - lastLatency : latency
        (lastFrames, lastBytes, lastLatency) = (frames, bytes, latency)
        let hostMs = df > 0 ? Double(dl) / Double(df) / 10 : 0
        let rtt = parts[0].control?.rttMs ?? 0
        func r1(_ x: Double) -> Double { (x * 10).rounded() / 10 }
        var sender: [String: Any] = ["t": "sender_stats", "rtt_ms": r1(rtt), "host_ms": r1(hostMs), "fps": df,
                                     "mbps": r1(Double(db) * 8 / 1e6)]
        if let plan {
            sender["encoder"] = plan.codec == .hevc ? "hevc_videotoolbox" : "h264_videotoolbox"
            sender["res"] = "\(plan.width)x\(plan.height)"
        }
        for p in live { p.control?.send(sender) }
        let decode = (primary["decode_ms"] as? NSNumber)?.doubleValue ?? 0
        let all: [String: Any] = ["sender": sender, "receiver": primary, "est_ms": r1(hostMs + rtt / 2 + decode),
                                  "group": group]
        lock.lock()
        last = all
        lock.unlock()
        onStats?(all)
    }

    /// ScreenCaptureKit is change-driven: on a still desktop the last access unit is sent again, as Engine does, so no
    /// receiver ends the session for want of a picture.
    private func keepalive() {
        guard let last = lastAU, Date().timeIntervalSince(lastSentAt) > Engine.keepaliveInterval(isIDR: last.flags.contains(.idr))
        else { return }
        let pts = CMClockGetTime(CMClockGetHostTimeClock())
        let bytes = [UInt8](last.data)
        for tx in live(\.video) { tx.send(frame: bytes, flags: last.flags, ptsUS: relativeUS(pts), hostLat: last.hostLat) }
        lastSentAt = Date()
    }

    // MARK: - time

    private func relativeUS(_ pts: CMTime) -> UInt32 {
        let us = (CMTimeGetSeconds(pts) - CMTimeGetSeconds(base)) * 1_000_000
        return UInt32(truncatingIfNeeded: max(0, Int64(us)))
    }

    private func hostLatency(_ pts: CMTime) -> UInt16 {
        let now = CMClockGetTime(CMClockGetHostTimeClock())
        let us = max(0, (CMTimeGetSeconds(now) - CMTimeGetSeconds(pts)) * 1_000_000)
        return UInt16(min(Double(UInt16.max), us / 100))
    }
}
