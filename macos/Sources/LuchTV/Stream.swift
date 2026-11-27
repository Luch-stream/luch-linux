// A blocking byte stream over an NWConnection (TLS for a Chromecast, for the web receiver's browsers): the threads
// that drive a TV read and write in order and wait anyway.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import Network

public final class ByteStream {
    let conn: NWConnection
    private let queue = DispatchQueue(label: "luch.tv.stream")
    private let cond = NSCondition()
    private var buffer = Data()
    private var reading = false
    private var failure: String?

    public init(_ conn: NWConnection) {
        self.conn = conn
    }

    /// TLS that takes the TV's self-signed certificate (a Chromecast's is signed by Google's device CA, which only
    /// the `deviceauth` challenge checks); the handshake is still signed with the TV's key.
    public static func tlsAnyCertificate() -> NWParameters {
        let tls = NWProtocolTLS.Options()
        sec_protocol_options_set_verify_block(tls.securityProtocolOptions, { _, _, complete in complete(true) },
                                              DispatchQueue.global())
        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        tcp.connectionTimeout = 5
        return NWParameters(tls: tls, tcp: tcp)
    }

    /// Connects and waits for the connection (and its handshake) to be ready.
    public static func connect(host: String, port: UInt16, parameters: NWParameters, timeout: Double) throws -> ByteStream {
        let s = ByteStream(NWConnection(host: NWEndpoint.Host(host), port: NWEndpoint.Port(rawValue: port)!, using: parameters))
        try s.start(timeout: timeout)
        return s
    }

    /// Starts an accepted or new connection and waits until it is ready.
    public func start(timeout: Double) throws {
        let ready = DispatchSemaphore(value: 0)
        var why: String?
        conn.stateUpdateHandler = { [weak self] st in
            switch st {
            case .ready: ready.signal()
            case .failed(let e), .waiting(let e):
                why = e.localizedDescription
                ready.signal()
                self?.fail(e.localizedDescription)
            case .cancelled: self?.fail("closed")
            default: break
            }
        }
        conn.start(queue: queue)
        guard ready.wait(timeout: .now() + timeout) == .success, why == nil else {
            conn.cancel()
            throw TVError(why ?? "no answer")
        }
    }

    private func fail(_ why: String) {
        cond.lock()
        if failure == nil { failure = why }
        cond.broadcast()
        cond.unlock()
    }

    /// Exactly `n` bytes; nil at the end of the stream, on an error or after `timeout` seconds.
    public func read(_ n: Int, timeout: Double = .infinity) -> Data? {
        let until = Date().addingTimeInterval(min(timeout, 1e9))
        cond.lock()
        defer { cond.unlock() }
        while buffer.count < n {
            if failure != nil { return nil }
            if !reading {
                reading = true
                conn.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) { [weak self] data, _, done, error in
                    guard let self else { return }
                    self.cond.lock()
                    self.reading = false
                    if let data { self.buffer.append(data) }
                    if let error { self.failure = self.failure ?? error.localizedDescription }
                    else if done && (data?.isEmpty ?? true) { self.failure = self.failure ?? "closed" }
                    self.cond.broadcast()
                    self.cond.unlock()
                }
            }
            if !cond.wait(until: until) { return nil }
        }
        let out = buffer.prefix(n)
        buffer.removeFirst(n)
        return Data(out)
    }

    /// Up to the first CRLF CRLF (an HTTP head), at most `max` bytes.
    public func readHead(max: Int = 16 * 1024, timeout: Double) -> Data? {
        var head = Data()
        while !head.suffix(4).elementsEqual([13, 10, 13, 10]) {
            guard head.count < max, let b = read(1, timeout: timeout) else { return nil }
            head.append(b)
        }
        return head
    }

    /// Sends and waits until it is handed to the network.
    @discardableResult
    public func write(_ d: Data) -> Bool {
        let done = DispatchSemaphore(value: 0)
        var ok = false
        conn.send(content: d, completion: .contentProcessed { e in
            ok = e == nil
            done.signal()
        })
        return done.wait(timeout: .now() + 10) == .success && ok
    }

    public func close() {
        conn.cancel()
        fail("closed")
    }

    /// The address of the other end, without its port.
    public var remoteAddress: String {
        if case let .hostPort(host, _) = conn.endpoint { return Self.text(host) }
        return ""
    }

    /// This end's address (the one the other side reached), without its port.
    public var localAddress: String {
        if case let .hostPort(host, _)? = conn.currentPath?.localEndpoint { return Self.text(host) }
        return ""
    }

    static func text(_ h: NWEndpoint.Host) -> String {
        switch h {
        case .ipv4(let a): return "\(a)".split(separator: "%").first.map(String.init) ?? ""
        case .ipv6(let a): return "\(a)".split(separator: "%").first.map(String.init) ?? ""
        case .name(let n, _): return n
        @unknown default: return ""
        }
    }
}
