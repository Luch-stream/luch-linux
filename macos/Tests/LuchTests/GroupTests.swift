// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import XCTest
@testable import LuchEngine
@testable import LuchNet
@testable import LuchProto

/// A receiver on loopback that plays in a group (PROTOCOL.md 4k): announces sync, reports its need and output route
/// every 200 ms, keeps what it is sent, and can ask the sender's clock on its media socket as the receiver does.
private final class FakeSyncReceiver {
    let name: String
    let port: UInt16
    var needMs: Double
    let route: String
    private let listener: Int32
    private let videoSock: Int32
    private let audioSock: Int32
    private let lock = NSLock()
    private var messages: [[String: Any]] = []
    private var packets: [[UInt8]] = []
    private var control: Int32 = -1
    private var senderAudio: sockaddr_in?

    init(name: String, needMs: Double, route: String = "builtin", sync: Bool = true) {
        self.name = name
        self.needMs = needMs
        self.route = route
        listener = socket(AF_INET, SOCK_STREAM, 0)
        port = bindLoopback(listener)
        listen(listener, 1)
        videoSock = socket(AF_INET, SOCK_DGRAM, 0)
        audioSock = socket(AF_INET, SOCK_DGRAM, 0)
        let (vp, ap) = (bindLoopback(videoSock), bindLoopback(audioSock))
        for s in [videoSock, audioSock] {
            var tv = timeval(tv_sec: 0, tv_usec: 200_000)
            setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
            // A strong reference only while a datagram is read: the fake goes away with its test.
            Thread.detachNewThread { [weak self] in while self?.readMedia(s) ?? false {} }
        }
        var welcome: [String: Any] = ["t": "welcome", "v": 0, "name": name, "modes": ["mirror", "audio"],
                                      "decoders": ["video/avc"], "hw": ["audio": ["audio/opus"]]]
        if sync { welcome["sync"] = ["v": 1, "clock": true, "probe": false, "mic": false] }
        Thread.detachNewThread { [weak self, listener] in
            let c = accept(listener, nil, nil)
            guard c >= 0, let self else { return }
            var one: Int32 = 1
            setsockopt(c, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
            self.lock.lock()
            self.control = c
            self.lock.unlock()
            let input = FileHandle(fileDescriptor: c, closeOnDealloc: false)
            var buffer = Data()
            func line() -> [String: Any]? {
                while true {
                    if let nl = buffer.firstIndex(of: 10) {
                        let one = buffer[buffer.startIndex..<nl]
                        buffer.removeSubrange(buffer.startIndex...nl)
                        return try? JSONSerialization.jsonObject(with: one) as? [String: Any]
                    }
                    let more = input.availableData
                    if more.isEmpty { return nil }
                    buffer.append(more)
                }
            }
            _ = line() // hello
            self.send(welcome)
            while let m = line() {
                self.lock.lock()
                self.messages.append(m)
                self.lock.unlock()
                if m["t"] as? String == "start" {
                    self.send(["t": "ready", "video_port": Int(vp), "audio_port": Int(ap)])
                    Thread.detachNewThread { [weak self] in
                        while self?.report() ?? false { Thread.sleep(forTimeInterval: 0.2) }
                    }
                }
            }
        }
    }

    deinit {
        [listener, videoSock, audioSock].forEach { close($0) }
        if control >= 0 { close(control) }
    }

    @discardableResult
    func send(_ o: [String: Any]) -> Bool {
        var d = try! JSONSerialization.data(withJSONObject: o)
        d.append(10)
        lock.lock()
        let c = control
        lock.unlock()
        return d.withUnsafeBytes { write(c, $0.baseAddress, d.count) } == d.count
    }

    /// One stats line; false once the sender is gone.
    private func report() -> Bool {
        send(["t": "stats", "need_ms": needMs, "out": ["route": route, "audio_ms": 20, "video_pts_ms": 30]])
    }

    /// One datagram (or a 0.2 s timeout); false once the socket is closed.
    private func readMedia(_ s: Int32) -> Bool {
        var b = [UInt8](repeating: 0, count: 2048)
        var from = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let n = withUnsafeMutablePointer(to: &from) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { recvfrom(s, &b, b.count, 0, $0, &len) }
        }
        if n < 0 && errno == EBADF { return false }
        guard n > 0 else { return true }
        lock.lock()
        packets.append(Array(b[0..<n]))
        if s == audioSock { senderAudio = from }
        lock.unlock()
        return true
    }

    /// Messages of type `t` received so far.
    func received(_ t: String) -> [[String: Any]] {
        lock.lock()
        defer { lock.unlock() }
        return messages.filter { $0["t"] as? String == t }
    }

    func media(kind: UInt8) -> [[UInt8]] {
        lock.lock()
        defer { lock.unlock() }
        return packets.filter { $0[0] == kind }
    }

    /// Asks the sender's clock as the receiver does: kind 3 on the audio socket, to where the sound comes from.
    func askClock(seq: UInt32, t1: UInt64) {
        lock.lock()
        guard var to = senderAudio else { lock.unlock(); return }
        lock.unlock()
        var req = Proto.header(kind: .audio, flags: [], shard: 0, shards: 0, seq: seq, ptsUS: UInt32(truncatingIfNeeded: t1),
                               frameLen: 8, hostLat: 0)
        req[0] = Sync.kindClock
        req += (0..<8).map { UInt8(truncatingIfNeeded: t1 >> (56 - 8 * UInt64($0))) }
        _ = withUnsafePointer(to: &to) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                sendto(audioSock, req, req.count, 0, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
    }

    var receiver: Receiver {
        Receiver(id: name.lowercased(), name: name, model: "", address: "127.0.0.1", interfaceName: "",
                 interfaceIndex: 0, port: port, service: "")
    }
}

private func eventually(_ seconds: Double = 4, _ check: () -> Bool) -> Bool {
    let until = Date().addingTimeInterval(seconds)
    while Date() < until {
        if check() { return true }
        Thread.sleep(forTimeInterval: 0.05)
    }
    return check()
}

final class GroupTests: XCTestCase {
    private var store: SyncStore!

    override func setUp() {
        signal(SIGPIPE, SIG_IGN)
        let d = UserDefaults(suiteName: "luch.tests.group")!
        d.removePersistentDomain(forName: "luch.tests.group")
        store = SyncStore(defaults: d)
    }

    private func options() -> MirrorOptions {
        var o = MirrorOptions()
        o.synthetic = true
        o.width = 640
        o.height = 360
        o.explicitSize = true
        return o
    }

    func testSpeakersInStepGetOneDFromTheSlowestNeedAndTheClock() throws {
        let kitchen = FakeSyncReceiver(name: "Kitchen", needMs: 200)
        let den = FakeSyncReceiver(name: "Den", needMs: 120, route: "hdmi_arc")
        let g = GroupSession(members: [GroupMember(kitchen.receiver, role: .both), GroupMember(den.receiver, role: .audio)],
                             mode: .watching, soundOnly: true, options: options(), store: store)
        try g.start()
        defer { g.stop(reason: "test") }

        // Each member's start: sound only, its part of one group.
        let starts = [kitchen, den].map { $0.received("start").first ?? [:] }
        for s in starts {
            XCTAssertEqual(s["mode"] as? String, "audio")
            XCTAssertNil(s["video"])
            let group = s["group"] as? [String: Any] ?? [:]
            XCTAssertEqual(group["role"] as? String, "audio", "sound only: every member a speaker")
            XCTAssertEqual(group["mode"] as? String, "watch")
            XCTAssertEqual(group["id"] as? String, g.id)
        }
        XCTAssertEqual((starts[0]["group"] as? [String: Any])?["timeline"] as? Int,
                       (starts[1]["group"] as? [String: Any])?["timeline"] as? Int)

        // One D for both: 10 ms above the slowest need (200), from one frame on everywhere.
        for tv in [kitchen, den] {
            XCTAssertTrue(eventually { tv.media(kind: 2).last.map { $0.be16(22) } == 2100 }, "\(tv.name): D 210 ms")
            let first = tv.media(kind: 2).first ?? []
            XCTAssertEqual(first[1] & PacketFlags.config.rawValue, PacketFlags.config.rawValue, "OpusHead first")
            XCTAssertEqual(first[1] & PacketFlags.timeline.rawValue, PacketFlags.timeline.rawValue, "a new timeline")
        }
        XCTAssertEqual(g.delay.audio, 210)

        // The clock, asked on the media socket.
        kitchen.askClock(seq: 9, t1: 0x0123456789ab)
        XCTAssertTrue(eventually { !kitchen.media(kind: Sync.kindClock).isEmpty })
        let reply = kitchen.media(kind: Sync.kindClock)[0]
        XCTAssertEqual(reply.count, Sync.replySize)
        XCTAssertEqual(reply[1], PacketFlags.clockReply.rawValue)
        XCTAssertEqual(reply.be32(8), 9)
        XCTAssertEqual(reply.be64(24), 0x0123456789ab)
        let (t2, t3) = (reply.be64(32), reply.be64(40))
        XCTAssertGreaterThan(t2, 0)
        XCTAssertGreaterThanOrEqual(t3, t2)
        XCTAssertLessThan(t2, 60_000_000, "on the session's pts clock, which started a moment ago")

        // Each member's route gets its correction at once: a soundbar's preset, none for the TV's own speakers.
        XCTAssertTrue(eventually { den.received("offset").first?["ms"] as? Double == 40 })
        XCTAssertEqual(kitchen.received("offset").first?["ms"] as? Double, 0)
        XCTAssertFalse(den.received("sender_stats").isEmpty)
        let members = (g.stats["group"] as? [String: Any])?["members"] as? [[String: Any]] ?? []
        XCTAssertEqual(members.map { $0["need_ms"] as? Double }, [200, 120])
        XCTAssertEqual(members.last?["offset_ms"] as? Double, 40)

        // `luchctl offset`: at once and kept for that route.
        XCTAssertTrue(g.setOffset(-12, for: "den"))
        XCTAssertTrue(eventually { den.received("offset").last?["ms"] as? Double == -12 })

        // A member leaving leaves the others playing.
        den.send(["t": "stop", "by": "receiver", "reason": "back"])
        Thread.sleep(forTimeInterval: 0.3)
        let before = den.media(kind: 2).count
        let kitchenBefore = kitchen.media(kind: 2).count
        Thread.sleep(forTimeInterval: 0.4)
        XCTAssertEqual(den.media(kind: 2).count, before, "nothing more to who left")
        XCTAssertGreaterThan(kitchen.media(kind: 2).count, kitchenBefore)

        g.stop(reason: "test")
        XCTAssertTrue(eventually { !kitchen.received("stop").isEmpty })
    }

    func testThePictureGoesOnlyToWhoShowsItAndTheSoundWaitsForIt() throws {
        let tv = FakeSyncReceiver(name: "Living room TV", needMs: 15)
        let speaker = FakeSyncReceiver(name: "Kitchen", needMs: 70)
        let g = GroupSession(members: [GroupMember(tv.receiver, role: .video), GroupMember(speaker.receiver, role: .audio)],
                             mode: .interactive, soundOnly: false, options: options(), store: store)
        try g.start()
        defer { g.stop(reason: "test") }
        XCTAssertEqual(tv.received("start").first?["mode"] as? String, "mirror")
        XCTAssertNil(tv.received("start").first?["audio"], "role video: no sound")
        XCTAssertEqual(speaker.received("start").first?["mode"] as? String, "audio")

        XCTAssertTrue(eventually { !tv.media(kind: 1).isEmpty && !speaker.media(kind: 2).isEmpty })
        XCTAssertTrue(tv.media(kind: 2).isEmpty)
        XCTAssertTrue(speaker.media(kind: 1).isEmpty)
        // Interactive: the speaker (70) is beyond the picture (30) plus the tolerance (20), so the picture waits 50.
        XCTAssertTrue(eventually { speaker.media(kind: 2).last.map { $0.be16(22) } == 700 })
        XCTAssertTrue(eventually { tv.media(kind: 1).last.map { $0.be16(22) } == 500 })
    }

    func testAReceiverThatCannotSyncIsRefusedAndNothingStarts() {
        let tv = FakeSyncReceiver(name: "Living room TV", needMs: 15)
        let old = FakeSyncReceiver(name: "Bedroom", needMs: 15, sync: false)
        let g = GroupSession(members: [GroupMember(tv.receiver, role: .both), GroupMember(old.receiver, role: .audio)],
                             mode: .watching, soundOnly: true, options: options(), store: store)
        XCTAssertThrowsError(try g.start()) { error in
            XCTAssertEqual(error.localizedDescription, "Bedroom cannot play in step yet: update it")
        }
        XCTAssertTrue(tv.received("start").isEmpty)
        XCTAssertTrue(old.received("start").isEmpty)
    }
}
