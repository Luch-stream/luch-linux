// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import Network
@testable import LuchNet

/// A receiver's TLS port as luch-android's ControlServer runs it (PROTOCOL.md 3a), on loopback: its own certificate,
/// every client's asked for, a paired one let in at once, an unknown one asked for the code and checked round by
/// round the receiver's way (luchd's pair::tests::receiver_side).
final class FakeTLSReceiver {
    let certificate: SenderCertificate
    private(set) var port: UInt16 = 0
    var code = "123456"
    /// Ask "allow?" before the code, as the TV does while something plays.
    var askFirst = false
    /// Sent right after the welcome to a sender that wants them.
    var reports: [[String: Any]] = []

    private let listener: NWListener
    private let queue = DispatchQueue(label: "fake.tls.receiver")
    private let lock = NSLock()
    private var pairedFPs: Set<String> = []
    private var heard: [[String: Any]] = []

    init(dir: URL) throws {
        certificate = try SenderCertificate.load(dir: dir)
        let tls = NWProtocolTLS.Options()
        let sec = tls.securityProtocolOptions
        sec_protocol_options_set_local_identity(sec, sec_identity_create(certificate.identity)!)
        sec_protocol_options_set_peer_authentication_required(sec, true)
        // Any one certificate gets through the handshake; whether it is paired is its fingerprint's business.
        sec_protocol_options_set_verify_block(sec, { _, _, complete in complete(true) }, queue)
        listener = try NWListener(using: NWParameters(tls: tls), on: .any)
        let ready = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { if case .ready = $0 { ready.signal() } }
        listener.newConnectionHandler = { [weak self] conn in self?.serve(conn) }
        listener.start(queue: queue)
        _ = ready.wait(timeout: .now() + 5)
        port = listener.port?.rawValue ?? 0
    }

    deinit { listener.cancel() }

    /// Every message a sender sent, pings left out.
    var messages: [[String: Any]] {
        lock.lock()
        defer { lock.unlock() }
        return heard
    }

    func isPaired(_ fp: Fingerprint) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return pairedFPs.contains(hex(fp))
    }

    private func serve(_ conn: NWConnection) {
        let ready = DispatchSemaphore(value: 0)
        conn.stateUpdateHandler = { if case .ready = $0 { ready.signal() } }
        conn.start(queue: DispatchQueue(label: "fake.tls.conn"))
        Thread.detachNewThread { [self] in
            guard ready.wait(timeout: .now() + 5) == .success, let peer = peerFingerprint(conn) else { return conn.cancel() }
            let link = Lines(TLSLink(conn))
            defer { link.link.close() }
            guard let hello = read(link), hello["t"] as? String == "hello" else { return }
            if !isPaired(peer) {
                if askFirst {
                    link.send(["t": "pair", "step": "allow"])
                    Thread.sleep(forTimeInterval: 0.3)
                }
                link.send(["t": "pair", "step": "code", "digits": 6])
                guard rounds(link, sender: peer) else { return }
                lock.lock()
                pairedFPs.insert(hex(peer))
                lock.unlock()
                link.send(["t": "paired"])
            }
            link.send(["t": "welcome", "v": 1, "id": "tv-1", "name": "Living room TV", "decoders": ["video/avc"],
                       "modes": ["mirror", "audio"], "hw": ["audio": ["audio/opus"]]])
            if !reports.isEmpty, (hello["want"] as? [String])?.contains("reports") == true {
                link.send(["t": "reports", "items": reports])
            }
            while let m = read(link) {
                // A session's media goes to `mediaPort`, sealed with `mediaKey` (PROTOCOL.md 4).
                if m["t"] as? String == "start" {
                    link.send(["t": "ready", "video_port": Int(mediaPort), "audio_port": Int(mediaPort),
                               "crypt": "aes-128-gcm", "key": Data(mediaKey).base64EncodedString()])
                }
            }
        }
    }

    var mediaPort: UInt16 = 0
    var mediaKey: [UInt8] = Array(repeating: 9, count: 16)

    /// A sender paired before (its certificate pinned here).
    func trust(_ fp: Fingerprint) {
        lock.lock()
        pairedFPs.insert(hex(fp))
        lock.unlock()
    }

    private func rounds(_ link: Lines, sender: Fingerprint) -> Bool {
        for (round, digit) in Array(code.utf8).enumerated() {
            guard let c = read(link), let theirs = (c["c"] as? String).flatMap(unhex) else { return false }
            let nonce = [UInt8](repeating: UInt8(round + 1), count: 16)
            let mine = Pairing.commitment(role: Pairing.receiver, round: round, digit: digit, nonce: nonce,
                                          sender: sender, receiver: certificate.fp)
            link.send(["t": "pair_commit", "round": round, "c": hex(mine)])
            guard let o = read(link), let n = (o["n"] as? String).flatMap(unhex) else { return false }
            if Pairing.commitment(role: Pairing.sender, round: round, digit: digit, nonce: n, sender: sender,
                                  receiver: certificate.fp) != theirs {
                link.send(["t": "pair_refused", "reason": "wrong_code"])
                return false
            }
            link.send(["t": "pair_open", "round": round, "n": hex(nonce)])
        }
        return true
    }

    /// The next message but pings, which are answered.
    private func read(_ link: Lines) -> [String: Any]? {
        while let m = link.next() {
            if m["t"] as? String == "ping" {
                link.send(["t": "pong", "n": m["n"] ?? 0])
                continue
            }
            if m["t"] as? String == "pong" { continue }
            lock.lock()
            heard.append(m)
            lock.unlock()
            return m
        }
        return nil
    }

    private func peerFingerprint(_ conn: NWConnection) -> Fingerprint? {
        guard let meta = conn.metadata(definition: NWProtocolTLS.definition) as? NWProtocolTLS.Metadata else { return nil }
        var der: [UInt8]?
        sec_protocol_metadata_access_peer_certificate_chain(meta.securityProtocolMetadata) { c in
            if der == nil { der = [UInt8](SecCertificateCopyData(sec_certificate_copy_ref(c).takeRetainedValue()) as Data) }
        }
        return der.map(fingerprint)
    }
}

/// JSON lines over a link.
final class Lines {
    let link: ControlLink
    private var pending = Data()

    init(_ link: ControlLink) { self.link = link }

    func next() -> [String: Any]? {
        while true {
            if let nl = pending.firstIndex(of: 0x0a) {
                let line = pending[pending.startIndex..<nl]
                pending.removeSubrange(pending.startIndex...nl)
                if let m = (try? JSONSerialization.jsonObject(with: Data(line))) as? [String: Any] { return m }
                continue
            }
            guard let chunk = try? link.read(), !chunk.isEmpty else { return nil }
            pending.append(chunk)
        }
    }

    func send(_ m: [String: Any]) {
        var d = try! JSONSerialization.data(withJSONObject: m)
        d.append(0x0a)
        _ = link.write(d)
    }
}
