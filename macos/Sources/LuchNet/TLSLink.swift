// TLS to a receiver (PROTOCOL.md 3, 3a) through Network.framework: this Mac's certificate as the client's, and the
// receiver's trusted for being the pinned one, not for a CA's signature (there is none on a LAN). The handshake still
// proves the receiver holds that certificate's key: the verify block replaces only the trust evaluation.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import Network
import Security

/// What a refused certificate says.
public let pinMismatch = "not the certificate this receiver was paired with"

public enum TLSError: Error, LocalizedError {
    /// The receiver showed another certificate than the pinned one.
    case pinMismatch
    case failed(String)

    public var errorDescription: String? {
        switch self {
        case .pinMismatch: return LuchNet.pinMismatch
        case let .failed(why): return why
        }
    }
}

final class TLSLink: ControlLink {
    private let conn: NWConnection
    private let lock = NSLock()
    private var ended = false

    /// Over a connection already started (tests accept one as a receiver would).
    init(_ conn: NWConnection) { self.conn = conn }

    static func connect(ip: String, port: UInt16, interfaceIndex: UInt32, identity: SecIdentity,
                        pinned: Fingerprint?, timeout: TimeInterval) throws -> (TLSLink, Fingerprint) {
        let queue = DispatchQueue(label: "luch.tls")
        let tls = NWProtocolTLS.Options()
        let sec = tls.securityProtocolOptions
        sec_protocol_options_set_min_tls_protocol_version(sec, .TLSv12)
        guard let me = sec_identity_create(identity) else { throw TLSError.failed("no TLS identity") }
        sec_protocol_options_set_local_identity(sec, me)
        let seen = Box<Fingerprint?>(nil)
        sec_protocol_options_set_verify_block(sec, { _, trust, complete in
            let chain = SecTrustCopyCertificateChain(sec_trust_copy_ref(trust).takeRetainedValue()) as? [SecCertificate]
            guard let leaf = chain?.first else { return complete(false) }
            let fp = fingerprint([UInt8](SecCertificateCopyData(leaf) as Data))
            seen.value = fp
            complete(pinned == nil || pinned == fp)
        }, queue)
        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        tcp.connectionTimeout = max(1, Int(timeout.rounded(.up)))
        let params = NWParameters(tls: tls, tcp: tcp)
        // Section 2: the interface the receiver was found on, never the default route (a VPN's).
        if interfaceIndex != 0, let i = nwInterface(index: interfaceIndex) { params.requiredInterface = i }
        guard let nwPort = NWEndpoint.Port(rawValue: port) else { throw TLSError.failed("bad port \(port)") }
        let conn = NWConnection(host: NWEndpoint.Host(ip), port: nwPort, using: params)

        let done = DispatchSemaphore(value: 0)
        let outcome = Box<String?>(nil)
        conn.stateUpdateHandler = { state in
            switch state {
            case .ready:
                done.signal()
            case let .failed(e), let .waiting(e):
                // Waiting is a refused or unreachable port: no use trying again here, the caller says so.
                if outcome.value == nil { outcome.value = e.localizedDescription }
                done.signal()
            default:
                break
            }
        }
        conn.start(queue: queue)
        let inTime = done.wait(timeout: .now() + timeout + 1) == .success
        conn.stateUpdateHandler = nil
        if let pinned, let fp = seen.value, fp != pinned {
            conn.cancel()
            throw TLSError.pinMismatch
        }
        guard inTime, outcome.value == nil, let fp = seen.value else {
            conn.cancel()
            throw TLSError.failed("TLS to \(ip):\(port): \(inTime ? outcome.value ?? "no certificate" : "timed out")")
        }
        return (TLSLink(conn), fp)
    }

    func write(_ data: Data) -> Bool {
        let sent = DispatchSemaphore(value: 0)
        let ok = Box(false)
        conn.send(content: data, completion: .contentProcessed { e in
            ok.value = e == nil
            sent.signal()
        })
        return sent.wait(timeout: .now() + 10) == .success && ok.value
    }

    func read() throws -> Data {
        lock.lock()
        let atEnd = ended
        lock.unlock()
        if atEnd { return Data() }
        let got = DispatchSemaphore(value: 0)
        let result = Box<Result<Data, Error>>(.success(Data()))
        conn.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, complete, error in
            if let data, !data.isEmpty {
                result.value = .success(data)
            } else if let error {
                result.value = .failure(LinkError(error.localizedDescription))
            }
            if complete {
                self?.lock.lock()
                self?.ended = true
                self?.lock.unlock()
            }
            got.signal()
        }
        got.wait()
        return try result.value.get()
    }

    func shutdownWrite() {
        conn.send(content: nil, contentContext: .finalMessage, isComplete: true, completion: .idempotent)
    }

    func close() {
        lock.lock()
        ended = true
        lock.unlock()
        conn.cancel()
    }

    var localAddress: String {
        guard case let .hostPort(host, _)? = conn.currentPath?.localEndpoint, case let .ipv4(a) = host else { return "" }
        return a.rawValue.map(String.init).joined(separator: ".")
    }
}

/// Network.framework's handle of the interface with this index, if it has one.
func nwInterface(index: UInt32) -> NWInterface? {
    let monitor = NWPathMonitor()
    let first = DispatchSemaphore(value: 0)
    let found = Box<NWInterface?>(nil)
    monitor.pathUpdateHandler = { path in
        found.value = path.availableInterfaces.first { $0.index == Int(index) }
        first.signal()
    }
    monitor.start(queue: DispatchQueue(label: "luch.interfaces"))
    _ = first.wait(timeout: .now() + 1)
    monitor.cancel()
    return found.value
}

/// A value a callback on another queue fills in; read after its semaphore.
final class Box<T> {
    private let lock = NSLock()
    private var v: T
    init(_ v: T) { self.v = v }
    var value: T {
        get { lock.lock(); defer { lock.unlock() }; return v }
        set { lock.lock(); v = newValue; lock.unlock() }
    }
}
