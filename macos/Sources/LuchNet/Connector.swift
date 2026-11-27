// Opening a control channel to a receiver the way luchd's Connect does (PROTOCOL.md 3, 3a): TLS with this Mac's
// certificate to a receiver that offers it, its certificate pinned once paired; the plain port only for a receiver
// that offers no TLS and was never paired. A receiver not paired yet shows a code: the connection then waits here for
// the code typed on this Mac (pair), and whatever connected fails with "... asks for pairing ...".
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchProto

public enum ConnectError: Error, LocalizedError {
    case failed(String)
    /// The receiver or this Mac said no (luchd's AccessDenied).
    case refused(String)
    /// A code is on the TV (or will be, once allowed there): `pair` it, then connect again.
    case pairingNeeded(id: String, name: String, allowFirst: Bool)

    public var errorDescription: String? {
        switch self {
        case let .failed(why), let .refused(why):
            return why
        case let .pairingNeeded(id, name, allowFirst):
            return allowFirst
                ? "\(name) asks for pairing: allow it on the TV, then enter the code it shows (`luchctl pair \(id)`)"
                : "\(name) asks for pairing: enter the code it shows (`luchctl pair \(id)`)"
        }
    }
}

/// A control channel with the receiver's welcome.
public struct Connection {
    public let channel: ControlChannel
    public let welcome: [String: Any]
    public var secure: Bool { channel.secure }
}

public final class Connector {
    /// The app's and the command line's; tests put in one with a directory of their own.
    public static var shared = Connector()

    /// Where the pins live (tests use their own).
    let dir: URL
    private let certificate: () throws -> SenderCertificate
    private let lock = NSLock()
    /// id -> where pairing stands: {"step": "allow"|"code"|"paired"|"failed", "name", "digits", "reason"}.
    private var states: [String: [String: Any]] = [:]
    /// Receivers waiting for their code, by id.
    private var waiting: [String: CodeSlot] = [:]

    /// Pairing with a receiver moved on (id, state as in `pairing`): "allow", "code", "paired" or "failed". Called on
    /// any thread; the app shows the code window from it, as GNOME does from luchd's PairChanged.
    public var onPairChanged: ((String, [String: Any]) -> Void)?

    init(dir: URL = supportDirectory(), certificate: @escaping () throws -> SenderCertificate = SenderCertificate.shared) {
        self.dir = dir
        self.certificate = certificate
    }

    /// Connects and says `hello` (built by the caller: v0, id, name, app, want); over TLS it becomes the v1 hello.
    /// `token`: the one-time pairing token from the receiver's QR code or sound (PROTOCOL.md 1e).
    public func open(_ r: Receiver, hello: [String: Any], token: String? = nil, timeout: TimeInterval = 5) throws -> Connection {
        lock.lock()
        let pending = waiting[r.id] != nil
        let step = states[r.id]?["step"] as? String
        lock.unlock()
        if pending { throw ConnectError.pairingNeeded(id: r.id, name: r.name, allowFirst: step == "allow") }

        let pinned = Pins.fingerprint(r.id, dir: dir)
        var me: SenderCertificate?
        if r.tlsPort > 0 {
            do {
                me = try certificate()
            } catch {
                log("tls: no certificate of this Mac (\(error.localizedDescription)): older receivers only")
            }
        }
        if me == nil && pinned != nil {
            // Paired once, so it spoke TLS: a receiver at its name that does not is not let pass.
            throw ConnectError.refused("\(r.name) was paired but now offers no encryption: refused (another device under "
                + "its name, or an older Luch on it). `luchctl forget \(r.id)` to use it unencrypted")
        }

        let ctl: ControlChannel
        var theirs: Fingerprint?
        if let me {
            do {
                (ctl, theirs) = try ControlChannel.openTLS(ip: r.address, port: r.tlsPort, interfaceIndex: r.interfaceIndex,
                                                          me: me, pinned: pinned, timeout: timeout)
            } catch TLSError.pinMismatch {
                throw ConnectError.refused("\(r.name): \(pinMismatch). If it was reset or Luch was reinstalled on it, "
                    + "`luchctl forget \(r.id)` and pair again")
            } catch {
                throw ConnectError.failed("\(r.name) (\(r.address)%\(r.interfaceName)): \(error.localizedDescription)")
            }
            ctl.onReports = { [weak ctl, dir] msg in
                if let ctl { Reports.take(msg, from: r, on: ctl, base: dir) }
            }
        } else {
            guard let plain = ControlChannel.open(ip: r.address, port: r.port, interfaceIndex: r.interfaceIndex,
                                                  timeout: timeout) else {
                throw ConnectError.failed("\(r.name) (\(r.address):\(r.port)): cannot connect")
            }
            ctl = plain
        }

        var h = hello
        if ctl.secure {
            h["v"] = 1
            // No pin yet: pair even if the receiver knows this Mac (it may have forgotten its key here).
            if pinned == nil { h["pair"] = true }
        }
        if let token { h["token"] = token }
        ctl.send(h)
        guard let first = ctl.waitForAny(["welcome", "pair", "pair_refused"], timeout: timeout) else {
            ctl.closeChannel("no welcome")
            throw ConnectError.failed("\(r.name): \(ctl.isClosed ? "closed before welcome" : "no welcome")")
        }
        switch first["t"] as? String ?? "" {
        case "welcome" where ctl.secure && pinned == nil:
            ctl.bye()
            throw ConnectError.refused("\(r.name) let this computer in without pairing: not trusted, nothing sent")
        case "welcome":
            log("connected to \(r.name) (\(r.address) via \(r.interfaceName)\(ctl.secure ? ", TLS" : ", not encrypted"))")
            return Connection(channel: ctl, welcome: first)
        case "pair":
            guard let me, let theirs else { throw ConnectError.failed("pairing asked for without TLS") }
            let slot = CodeSlot()
            let state = Self.state(first, name: r.name)
            lock.lock()
            waiting[r.id] = slot
            lock.unlock()
            changed(r.id, state)
            Thread.detachNewThread { [self] in
                pairing(r, ctl, first: first, me: me.fp, theirs: theirs, slot: slot)
            }
            throw ConnectError.pairingNeeded(id: r.id, name: r.name, allowFirst: first["step"] as? String == "allow")
        default:
            ctl.closeChannel("refused")
            throw ConnectError.refused(Pairing.explain(Pairing.refused(first), tv: r.name))
        }
    }

    /// Who answers on a receiver's plain port: the welcome of a probe hello (PROTOCOL.md 1b), which shows nothing
    /// on the TV. Its `tls_port` is the TLS port of a receiver found without its TXT record.
    public static func probe(_ r: Receiver, id: String, name: String, timeout: TimeInterval = 2) -> [String: Any]? {
        guard let ctl = ControlChannel.open(ip: r.address, port: r.port, interfaceIndex: r.interfaceIndex,
                                            timeout: timeout) else { return nil }
        defer { ctl.bye() }
        ctl.send(["t": "hello", "v": Int(Proto.version), "id": id, "name": name, "app": Proto.app, "want": [String](),
                  "probe": true])
        return ctl.waitFor("welcome", timeout: timeout)
    }

    // MARK: - pairing

    /// Where pairing with a receiver stands: `step` "none", "allow" (waiting for the TV's allow), "code" (a code is on
    /// the TV: pair), "paired" or "failed" (with `reason` in words), and `name`, `digits`.
    public func pairing(_ id: String) -> [String: Any] {
        lock.lock()
        defer { lock.unlock() }
        return states[id] ?? ["step": "none"]
    }

    /// The code shown on a receiver that asked for pairing, as typed: six digits, spaces allowed. Both sides then pin
    /// each other's certificate; returns the receiver's name. A wrong code ends this pairing: connect again for a new
    /// code.
    @discardableResult
    public func pair(_ id: String, code typed: String) throws -> String {
        let code: [UInt8]
        do {
            code = try Pairing.cleanCode(typed)
        } catch Pairing.Failed.io(let why) {
            throw ConnectError.failed(why)
        }
        lock.lock()
        let slot = waiting[id]
        let step = states[id]?["step"] as? String
        lock.unlock()
        guard let slot else { throw ConnectError.failed("no pairing with \(id) going on: connect to it first") }
        guard step == "code" else { throw ConnectError.failed("no code on the TV yet: allow the connection there first") }
        guard let result = slot.enter(code, wait: 20) else {
            throw ConnectError.failed("the receiver did not finish pairing")
        }
        switch result {
        case let .success(name): return name
        case let .failure(why): throw ConnectError.refused(why.description)
        }
    }

    /// The receivers this Mac is paired with: {id, name, fp, paired (unix time)}, by name.
    public func paired() -> [[String: Any]] {
        Pins.all(dir: dir).map { id, v in v.merging(["id": id]) { $1 } }
            .sorted { ($0["name"] as? String ?? "") < ($1["name"] as? String ?? "") }
    }

    public func isPaired(_ id: String) -> Bool { Pins.fingerprint(id, dir: dir) != nil }

    /// Forgets a receiver's pinned certificate: the next connection pairs again with a new code.
    public func forget(_ id: String) throws {
        let was: Bool
        do {
            was = try Pins.forget(id, dir: dir)
        } catch {
            throw ConnectError.failed(error.localizedDescription)
        }
        if !was { throw ConnectError.failed("not paired with \(id)") }
    }

    /// A receiver asking for pairing: waits for its allow (when it asks that first) and for the code typed here,
    /// runs the code check and pins its certificate. Gives up after two minutes. The channel is closed after: the
    /// command that found the receiver asking connects again, now paired.
    private func pairing(_ r: Receiver, _ ctl: ControlChannel, first: [String: Any], me: Fingerprint, theirs: Fingerprint,
                         slot: CodeSlot) {
        let name = r.name
        let deadline = Date().addingTimeInterval(130)
        var msg = first
        let result: Result<String, PairFailure> = {
            while true {
                if msg["t"] as? String == "pair_refused" {
                    return .failure(PairFailure(Pairing.explain(Pairing.refused(msg), tv: name)))
                }
                guard msg["t"] as? String == "pair" else { return .failure(PairFailure("\(name): unexpected \(msg)")) }
                changed(r.id, Self.state(msg, name: name))
                if msg["step"] as? String == "allow" {
                    // The TV asks its person first; its next word is the code step or a refusal.
                    guard let next = ctl.waitForAny(["pair", "pair_refused"], timeout: deadline.timeIntervalSinceNow) else {
                        return .failure(PairFailure(ctl.isClosed ? "\(name) closed the connection"
                                                                 : "nobody answered on \(name) in time"))
                    }
                    msg = next
                    continue
                }
                // The code is on the TV: wait for it from here, or for the TV to give up.
                var code: [UInt8]?
                while code == nil {
                    if let refusal = ctl.waitForAny(["pair_refused"], timeout: 0.2) {
                        return .failure(PairFailure(Pairing.explain(Pairing.refused(refusal), tv: name)))
                    }
                    if ctl.isClosed {
                        return .failure(PairFailure(Pairing.explain(.refused("refused", 0), tv: name)))
                    }
                    if Date() > deadline {
                        ctl.bye()
                        return .failure(PairFailure("no code was entered for \(name) in time"))
                    }
                    code = slot.code
                }
                do {
                    try Pairing.codeCheck(ctl, code: code!, me: me, receiver: theirs)
                } catch let f as Pairing.Failed {
                    return .failure(PairFailure(Pairing.explain(f, tv: name)))
                } catch {
                    return .failure(PairFailure(error.localizedDescription))
                }
                guard ctl.waitFor("welcome", timeout: 5) != nil else {
                    return .failure(PairFailure("\(name) paired but sent no welcome"))
                }
                return .success(name)
            }
        }()
        lock.lock()
        waiting[r.id] = nil
        lock.unlock()
        switch result {
        case .success:
            do {
                try Pins.pin(r.id, fp: theirs, name: name, dir: dir)
            } catch {
                log("pairing: cannot keep the pairing with \(name): \(error.localizedDescription)")
            }
            log("paired with \(name)")
            ctl.bye()
            changed(r.id, ["step": "paired", "name": name])
        case let .failure(why):
            log("pairing with \(name): \(why)")
            ctl.closeChannel("pairing ended")
            changed(r.id, ["step": "failed", "name": name, "reason": why.description])
        }
        slot.finish(result)
    }

    private func changed(_ id: String, _ state: [String: Any]) {
        lock.lock()
        states[id] = state
        lock.unlock()
        onPairChanged?(id, state)
    }

    /// What `pairing` says about a receiver's `pair` message.
    static func state(_ msg: [String: Any], name: String) -> [String: Any] {
        ["step": msg["step"] as? String ?? "", "digits": (msg["digits"] as? NSNumber)?.intValue ?? Pairing.digits,
         "name": name]
    }
}

struct PairFailure: Error, CustomStringConvertible {
    let description: String
    init(_ why: String) { description = why }
}

/// Where the code typed here meets the connection waiting for it, and the outcome goes back.
final class CodeSlot {
    private let cond = NSCondition()
    private var typed: [UInt8]?
    private var result: Result<String, PairFailure>?

    var code: [UInt8]? {
        cond.lock()
        defer { cond.unlock() }
        return typed
    }

    /// Hands the code over and waits for the outcome; nil when none came in time.
    func enter(_ code: [UInt8], wait: TimeInterval) -> Result<String, PairFailure>? {
        let until = Date().addingTimeInterval(wait)
        cond.lock()
        defer { cond.unlock() }
        if typed == nil { typed = code }
        while result == nil && Date() < until { cond.wait(until: until) }
        return result
    }

    func finish(_ r: Result<String, PairFailure>) {
        cond.lock()
        result = r
        cond.broadcast()
        cond.unlock()
    }
}
