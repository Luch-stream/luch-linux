// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import XCTest
@testable import LuchEngine
@testable import LuchLocal
@testable import LuchNet
@testable import LuchProto

private func hex(_ s: String) -> [UInt8] {
    let c = Array(s.utf8)
    return stride(from: 0, to: c.count, by: 2).map { UInt8(String(decoding: c[$0..<$0 + 2], as: UTF8.self), radix: 16)! }
}

/// luchd/src/sync.rs's vectors, which are also the receiver's (WireTest, SenderPolicyTest): every sender and the
/// receiver read the same bytes and pick the same D.
final class SyncTests: XCTestCase {
    // Request 7 sent at µs 0x0123456789ab, answered with t2 = 1 000 000 and t3 = 1 000 150.
    static let request = "0300" + "0000" + "0000" + "0000" + "00000007" + "456789ab" + "00000008" + "0000" + "0000"
        + "00000123456789ab"
    static let reply = "0310" + "0000" + "0000" + "0000" + "00000007" + "456789ab" + "00000018" + "0000" + "0000"
        + "00000123456789ab" + "00000000000f4240" + "00000000000f42d6"

    func testClockPacketsAreTheReceiversBytes() {
        let req = hex(Self.request)
        let parsed = Sync.parseRequest(req)
        XCTAssertEqual(parsed?.seq, 7)
        XCTAssertEqual(parsed?.t1, 0x0123456789ab)
        XCTAssertEqual(Sync.reply(to: req, t2: 1_000_000, t3: 1_000_150), hex(Self.reply))
        // A reply is not a request, a short packet is nothing.
        XCTAssertNil(Sync.parseRequest(hex(Self.reply)))
        XCTAssertNil(Sync.parseRequest(Array(req[..<30])))
        // Before the pts clock runs there is nothing to answer with.
        XCTAssertNil(Sync.answer(req, now: { nil }))
    }

    func testHeaderUnitsAreTenthsOfAMillisecond() {
        XCTAssertEqual(Sync.headerUnits(150.5), 1505)
        XCTAssertEqual(Sync.headerUnits(9000), UInt16.max)
        XCTAssertEqual(Sync.headerUnits(-3), 0)
    }

    static let n = Double.nan
    /// {now s, video pts ms, need ms, D_v, D_a}.
    static let watching: [[Double]] = [
        [0, n, n, 150, 150],
        [1, 40, 120, 150, 150],
        [2, 40, 210, 220, 220],
        [3, 40, 230.5, 240.5, 240.5],
        [4, 40, 200, 240.5, 240.5],
        [33, 40, 200, 240.5, 240.5],
        [34, 40, 200, 210, 210],
        [35, 40, 200, 210, 210],
        [36, 40, 229, 239, 239],
        [37, 40, 240, 250, 250],
    ]
    static let interactive: [[Double]] = [
        [0, 30, 15, 0, 30],
        [1, 31, 15, 0, 30],
        [2, 33, 15, 0, 33],
        [3, 25, 15, 0, 33],
        [13, 25, 15, 0, 25],
        [14, 25, 70, 50, 70],
        [15, 25, 40, 50, 70],
    ]

    private func run(_ policy: SyncPolicy, _ rows: [[Double]]) {
        var p = policy
        for r in rows {
            let d = p.update(nowS: r[0], videoPtsMs: [r[1]], needMs: [r[2]])
            XCTAssertEqual(d.video, r[3], "D_v at \(r[0]) s")
            XCTAssertEqual(d.audio, r[4], "D_a at \(r[0]) s")
        }
    }

    func testPolicyWatching() {
        run(SyncPolicy(mode: .watching, toleranceMs: 0, floorMs: 150), Self.watching)
    }

    func testPolicyInteractive() {
        run(SyncPolicy(mode: .interactive, toleranceMs: 20, floorMs: 0), Self.interactive)
    }

    func testOnlyReceiversThatSaySoAreCapable() {
        XCTAssertTrue(Sync.capable(["sync": ["v": 1, "clock": true]]))
        XCTAssertFalse(Sync.capable(["sdk": 34]))
        XCTAssertFalse(Sync.capable(["sync": ["v": 1, "clock": false]]))
    }

    func testReportsComeFromStats() throws {
        let s = try JSONSerialization.jsonObject(with: Data("""
            {"t":"stats","need_ms":45.5,"out":{"video_pts_ms":31.2,"route":"hdmi_arc","audio_ms":22.0}}
            """.utf8)) as! [String: Any]
        let r = SyncReport(stats: s)
        XCTAssertEqual(r.needMs, 45.5)
        XCTAssertEqual(r.videoPtsMs, 31.2)
        XCTAssertEqual(Sync.routePresetMs(r.route!, audioMs: r.audioMs), 40)
        XCTAssertEqual(SyncReport(stats: ["t": "stats"]), SyncReport())
    }

    func testPresetsByRoute() {
        XCTAssertEqual(Sync.routePresetMs("builtin", audioMs: 20), 0)
        // A Bluetooth speaker whose delay report reaches the timestamps looks slow already: no preset on top.
        XCTAssertEqual(Sync.routePresetMs("bluetooth", audioMs: 210), 0)
        XCTAssertEqual(Sync.routePresetMs("bluetooth", audioMs: 30), 150)
        XCTAssertEqual(Sync.routePresetMs("bluetooth", audioMs: nil), 0)
        XCTAssertEqual(Sync.offsetKey("tv-1", route: "hdmi_arc"), "tv-1@hdmi_arc")
    }

    func testGroupBlockIsTheProtocols() {
        let g = Sync.groupBlock(id: "g1", timeline: 7, role: .audio, mode: .interactive, toleranceMs: 20, offsetMs: -3.5)
        XCTAssertEqual(g as NSDictionary, ["id": "g1", "timeline": 7, "role": "audio", "mode": "interactive",
                                           "tolerance_ms": 20.0, "offset_ms": -3.5] as NSDictionary)
        XCTAssertEqual(SyncMode.parse("watch"), .watching)
        XCTAssertEqual(SyncMode.watching.wire, "watch")
        XCTAssertNil(SyncMode.parse("loud"))
    }

    // luchd/src/limits.rs's: the sound mode decides the start's audio block.
    func testAutoSyncsOnlyWhereSoundLags() {
        XCTAssertEqual(AudioMode.auto.startBlock(limits: ["audio_lag"], frameMs: 5)?["sync"] as? Bool, true)
        XCTAssertNil(AudioMode.auto.startBlock(limits: [], frameMs: 5)?["sync"])
        XCTAssertNil(AudioMode.off.startBlock(limits: ["audio_lag"], frameMs: 5))
        XCTAssertNil(AudioMode.on.startBlock(limits: ["audio_lag"], frameMs: 5)?["sync"])
        XCTAssertEqual(AudioMode.sync.startBlock(limits: [], frameMs: 5)?["sync"] as? Bool, true)
        XCTAssertEqual(AudioMode.on.startBlock(limits: [], frameMs: 20)?["frame_ms"] as? Int, 20)
        let caps = ReceiverCaps(welcome: ["t": "welcome", "limits": ["audio_latency_ms": 120, "limits": ["audio_lag"]]])
        XCTAssertEqual(caps?.limits, ["audio_lag"])
    }

    func testCorrectionsAreKeptPerRoute() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "luch.tests.sync"))
        defaults.removePersistentDomain(forName: "luch.tests.sync")
        let store = SyncStore(defaults: defaults)
        XCTAssertEqual(store.route("tv"), "unknown")
        XCTAssertEqual(store.currentOffset("tv"), 0)
        store.rememberRoute("hdmi_arc", for: "tv")
        store.setOffset(35, key: Sync.offsetKey("tv", route: "hdmi_arc"))
        XCTAssertEqual(store.currentOffset("tv"), 35)
        store.rememberRoute("builtin", for: "tv")
        XCTAssertEqual(store.currentOffset("tv"), 0)
        XCTAssertEqual(store.audioMode("tv"), .auto)
        store.setAudioMode(.sync, for: "tv")
        XCTAssertEqual(store.audioMode("tv"), .sync)
        defaults.removePersistentDomain(forName: "luch.tests.sync")
    }

    // luchctl/src/main.rs's group_arguments.
    func testGroupArguments() throws {
        let (members, source) = try GroupArgs.parse(["Living room TV", "Kitchen=audio", "--source", "monitor"])
        XCTAssertEqual(members.map(\.0), ["Living room TV", "Kitchen"])
        XCTAssertEqual(members.map(\.1), [nil, "audio"])
        XCTAssertEqual(source, "monitor")
        XCTAssertThrowsError(try GroupArgs.parse(["TV"]))
        XCTAssertThrowsError(try GroupArgs.parse(["TV", "B=loud"]))
        XCTAssertEqual(try GroupArgs.parse(["TV=both", "Den=video"]).members[1].1, "video")
        XCTAssertEqual(try GroupArgs.parse(["TV", "Den"]).source, "fast")
    }

    /// A receiver on loopback: what the sender's media socket sends it, and a clock request back on the same flow.
    func testPacketsCarryDAndClockRequestsAreAnsweredOnTheMediaSocket() throws {
        let rx = socket(AF_INET, SOCK_DGRAM, 0)
        defer { close(rx) }
        let port = bindLoopback(rx)
        var tv = timeval(tv_sec: 2, tv_usec: 0)
        setsockopt(rx, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        let tx = try XCTUnwrap(UdpSender.open(kind: .audio, ip: "127.0.0.1", port: port, interfaceIndex: 0, dscp: 48))
        tx.startTimeline()
        tx.setDelay(Sync.headerUnits(150.5))
        tx.send(frame: [1, 2, 3], flags: [], ptsUS: 42, hostLat: 3)
        tx.setDelay(Sync.headerUnits(210))
        tx.send(frame: [4, 5, 6], flags: [], ptsUS: 43, hostLat: 3)

        var from = sockaddr_in()
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        var buf = [UInt8](repeating: 0, count: 256)
        let n = withUnsafeMutablePointer(to: &from) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { recvfrom(rx, &buf, buf.count, 0, $0, &len) }
        }
        XCTAssertEqual(n, Proto.headerSize + 3)
        XCTAssertEqual(buf[1], PacketFlags.timeline.rawValue, "the first frame says its pts start anew")
        XCTAssertEqual(buf.be16(22), 1505)
        let second = recv(rx, &buf, buf.count, 0)
        XCTAssertEqual(second, Proto.headerSize + 3)
        XCTAssertEqual(buf[1], 0, "only the first")
        XCTAssertEqual(buf.be16(22), 2100, "D changes from the next frame on")

        let clock = ClockResponder(on: tx, now: { 1_000_000 })
        defer { clock.stop() }
        let req = hex(Self.request)
        _ = withUnsafePointer(to: &from) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                sendto(rx, req, req.count, 0, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        let got = recv(rx, &buf, buf.count, 0)
        XCTAssertEqual(got, Sync.replySize)
        XCTAssertEqual(Array(buf[0..<Sync.replySize]), Sync.reply(to: req, t2: 1_000_000, t3: 1_000_000))
    }
}

/// Binds to a free port of 127.0.0.1 and returns it.
func bindLoopback(_ s: Int32) -> UInt16 {
    var a = sockaddr_in()
    a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
    a.sin_family = sa_family_t(AF_INET)
    a.sin_addr.s_addr = inet_addr("127.0.0.1")
    withUnsafePointer(to: &a) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { _ = bind(s, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
    }
    var len = socklen_t(MemoryLayout<sockaddr_in>.size)
    withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { _ = getsockname(s, $0, &len) } }
    return UInt16(bigEndian: a.sin_port)
}
