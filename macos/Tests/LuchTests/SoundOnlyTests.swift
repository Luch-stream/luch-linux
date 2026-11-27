// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import XCTest
@testable import LuchEngine
@testable import LuchNet
@testable import LuchProto

/// A receiver on loopback that speaks the control protocol and records what arrives (PROTOCOL.md 4d).
private final class FakeReceiver {
    let port: UInt16
    let audioPort: UInt16
    private let listener: Int32
    private let udp: Int32
    var start: [String: Any]?
    private let started = DispatchSemaphore(value: 0)

    init(modes: [String]) {
        listener = socket(AF_INET, SOCK_STREAM, 0)
        var one: Int32 = 1
        setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &one, socklen_t(MemoryLayout<Int32>.size))
        port = FakeReceiver.bind(listener)
        listen(listener, 1)
        udp = socket(AF_INET, SOCK_DGRAM, 0)
        audioPort = FakeReceiver.bind(udp)
        var tv = timeval(tv_sec: 3, tv_usec: 0)
        setsockopt(udp, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        let welcome: [String: Any] = ["t": "welcome", "v": 1, "name": "Living room TV", "modes": modes,
                                      "hw": ["audio": ["audio/opus"]]]
        let audioPort = self.audioPort
        Thread.detachNewThread { [self] in
            let c = accept(listener, nil, nil)
            guard c >= 0 else { return }
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
            func send(_ o: [String: Any]) {
                var d = try! JSONSerialization.data(withJSONObject: o)
                d.append(10)
                _ = d.withUnsafeBytes { write(c, $0.baseAddress, d.count) }
            }
            _ = line() // hello
            send(welcome)
            while let m = line() {
                if m["t"] as? String == "start" {
                    self.start = m
                    send(["t": "ready", "audio_port": Int(audioPort)])
                    self.started.signal()
                }
            }
            close(c)
        }
    }

    deinit {
        close(listener)
        close(udp)
    }

    private static func bind(_ s: Int32) -> UInt16 {
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        a.sin_addr.s_addr = inet_addr("127.0.0.1")
        withUnsafePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { _ = Darwin.bind(s, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { _ = getsockname(s, $0, &len) } }
        return UInt16(bigEndian: a.sin_port)
    }

    func waitStart() -> Bool { started.wait(timeout: .now() + 5) == .success }

    /// The next audio datagram, or nil after 3 s of silence on the wire.
    func packet() -> [UInt8]? {
        var b = [UInt8](repeating: 0, count: 2048)
        let n = recv(udp, &b, b.count, 0)
        return n > 0 ? Array(b[0..<n]) : nil
    }

    var receiver: Receiver {
        Receiver(id: "test", name: "Living room TV", model: "", address: "127.0.0.1", interfaceName: "",
                 interfaceIndex: 0, port: port, service: "")
    }
}

final class SoundOnlyTests: XCTestCase {
    private func options() -> MirrorOptions {
        var o = MirrorOptions()
        o.soundOnly = true
        o.synthetic = true
        return o
    }

    func testSoundOnlyStartsWithoutVideoAndSendsConfigFirst() throws {
        let tv = FakeReceiver(modes: ["mirror", "audio"])
        let engine = Engine(options: options())
        try engine.start(receiver: tv.receiver)
        defer { engine.stop(reason: "test") }
        XCTAssertTrue(tv.waitStart())
        XCTAssertEqual(tv.start?["mode"] as? String, "audio")
        XCTAssertNil(tv.start?["video"], "sound only sends no video block")
        let audio = tv.start?["audio"] as? [String: Any]
        XCTAssertEqual(audio?["codec"] as? String, "audio/opus")

        let first = try XCTUnwrap(tv.packet())
        XCTAssertEqual(first[0], MediaKind.audio.rawValue)
        XCTAssertEqual(first[1], PacketFlags.config.rawValue, "the decoder's config goes first")
        XCTAssertEqual(Array(first[Proto.headerSize...].prefix(8)), Array("OpusHead".utf8))

        // Then Opus at the real pace: 20 ms frames, so ~25 in half a second.
        var count = 0
        let until = Date().addingTimeInterval(0.5)
        while Date() < until, let p = tv.packet() {
            XCTAssertEqual(p[0], MediaKind.audio.rawValue)
            XCTAssertEqual(p[1], 0)
            count += 1
        }
        XCTAssertGreaterThan(count, 15)
        XCTAssertLessThan(count, 40)
    }

    func testAReceiverWithoutSoundOnlyIsRefused() {
        let tv = FakeReceiver(modes: ["mirror"])
        let engine = Engine(options: options())
        XCTAssertThrowsError(try engine.start(receiver: tv.receiver)) { error in
            guard case .noSoundOnly? = error as? EngineError else { return XCTFail("\(error)") }
        }
        XCTAssertNil(tv.start, "no start is sent")
    }
}
