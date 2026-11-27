// SPDX-License-Identifier: GPL-3.0-or-later
import CryptoKit
import Darwin
import XCTest
import LuchNet
@testable import LuchProto

/// Sealed media (PROTOCOL.md 4) with the vectors luchd and the receiver share.
final class SealTests: XCTestCase {
    let key: [UInt8] = Array(0..<16)

    /// The same bytes as luchd's mirror::tests::a_frame_is_sealed_as_the_receiver_opens_it and the receiver's
    /// MediaCryptTest.sealedTheSameOnEveryImplementation.
    func testAFrameIsSealedAsTheReceiverOpensIt() {
        let plain = Array("luch frame".utf8)
        // Video, IDR, shard 3 of 1 (the index is not authenticated), seq 7, pts 1000, host latency 0.5 ms.
        let h = Proto.header(kind: .video, flags: .idr, shard: 3, shards: 1, seq: 7, ptsUS: 1000,
                             frameLen: UInt32(plain.count + MediaSeal.tag), hostLat: 5)
        let sealed = MediaSeal(key: key)!.seal(header: h, frame: plain)
        XCTAssertEqual(hex(sealed), "c88e092ce36d77f6a5954720c3a59966db75e601486ef8e09989")
    }

    func testTheMediaKeyComesFromReady() throws {
        XCTAssertNil(try MediaSeal.key(ready: ["t": "ready"]))
        XCTAssertEqual(try MediaSeal.key(ready: ["crypt": "aes-128-gcm", "key": "AAECAwQFBgcICQoLDA0ODw=="]), key)
        XCTAssertThrowsError(try MediaSeal.key(ready: ["crypt": "aes-128-gcm", "key": "AAEC"]))
        XCTAssertThrowsError(try MediaSeal.key(ready: ["crypt": "chacha", "key": "AAEC"]))
    }

    /// Sealed frames go out sharded like plain ones, the length counting the tag; opened the receiver's way (the
    /// header as received, index and parity bit cleared), they are the frame. Audio is sealed the same way.
    func testASealedFrameIsShardedAndOpens() throws {
        let rx = socket(AF_INET, SOCK_DGRAM, 0)
        defer { close(rx) }
        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        withUnsafeMutablePointer(to: &addr) { p in
            p.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                _ = Darwin.bind(rx, $0, len)
                _ = getsockname(rx, $0, &len)
            }
        }
        var tv = timeval(tv_sec: 2, tv_usec: 0)
        setsockopt(rx, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        let port = UInt16(bigEndian: addr.sin_port)

        for kind in [MediaKind.video, .audio] {
            let tx = try XCTUnwrap(UdpSender.open(kind: kind, ip: "127.0.0.1", port: port, interfaceIndex: 0, dscp: 40,
                                                  seal: MediaSeal(key: key)))
            tx.send(frame: [9], flags: [], ptsUS: 1, hostLat: 1) // seq 0: the next frame is seq 1
            var buf = [UInt8](repeating: 0, count: 2000)
            _ = recv(rx, &buf, buf.count, 0)
            let frame = (0..<(Proto.shardSize + 10)).map { UInt8(truncatingIfNeeded: $0) }
            tx.send(frame: frame, flags: .idr, ptsUS: 1234, hostLat: 5)
            var got: [UInt8] = []
            var first: [UInt8] = []
            for i in 0..<2 {
                let n = recv(rx, &buf, buf.count, 0)
                XCTAssertGreaterThan(n, Proto.headerSize)
                let h = Array(buf[0..<Proto.headerSize])
                XCTAssertEqual(h.be16(2), UInt16(i))
                XCTAssertEqual(h.be16(4), 2)
                if i == 0 { first = h }
                got += buf[Proto.headerSize..<n]
            }
            XCTAssertEqual(Int(first.be32(16)), frame.count + MediaSeal.tag)
            XCTAssertNotEqual(Array(got.prefix(frame.count)), frame, "on the wire it is not the frame")
            var aad = first
            aad[1] &= ~PacketFlags.parity.rawValue
            aad[2] = 0
            aad[3] = 0
            var nonce = [UInt8](repeating: 0, count: 12)
            nonce[0] = kind.rawValue
            nonce[4..<8] = first[8..<12]
            XCTAssertEqual(first.be32(8), 1)
            let box = try AES.GCM.SealedBox(nonce: AES.GCM.Nonce(data: nonce), ciphertext: got.dropLast(16),
                                            tag: got.suffix(16))
            let plain = try AES.GCM.open(box, using: SymmetricKey(data: key), authenticating: aad)
            XCTAssertEqual([UInt8](plain), frame)
        }
    }

    /// One seal per frame: an IDR of 200 KB, the biggest a 1080p session sends, is what it costs at most.
    func testSealingAnIDRIsCheap() {
        let seal = MediaSeal(key: key)!
        let idr = (0..<200_000).map { UInt8(truncatingIfNeeded: $0 &* 31) }
        let h = Proto.header(kind: .video, flags: .idr, shard: 0, shards: 146, seq: 1, ptsUS: 0,
                             frameLen: UInt32(idr.count + MediaSeal.tag), hostLat: 0)
        _ = seal.seal(header: h, frame: idr)
        let runs = 50
        let t0 = DispatchTime.now().uptimeNanoseconds
        for _ in 0..<runs { _ = seal.seal(header: h, frame: idr) }
        let us = Double(DispatchTime.now().uptimeNanoseconds - t0) / Double(runs) / 1000
        print(String(format: "seal: a 200 KB IDR in %.0f µs (AES-128-GCM, CryptoKit)", us))
        XCTAssertLessThan(us, 5000)
    }
}
