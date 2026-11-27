// The receiver's input on this Mac (PROTOCOL.md 4i, luch-linux docs/input.md): the person's permission, the datagrams
// on the video flow, and the injection. Nothing is injected before a grant.
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreGraphics
import Foundation
import LuchNet
import LuchProto

public enum InputAnswer: String {
    case once, always, no
}

/// One session's permission, as luchd's `Input`: asked once, the answer holds for the session.
public struct InputGate {
    public enum Step: Equatable {
        /// Answer the receiver at once with this.
        case reply(InputAnswer)
        /// Ask the person, then call `answer`.
        case ask
        /// A question is already on the screen.
        case wait
    }

    public let canInject: Bool
    public private(set) var answered: InputAnswer?
    public private(set) var granted = false
    private var asking = false

    public init(canInject: Bool) { self.canInject = canInject }

    /// At the start of a session: a receiver the person said "always" to is granted without a request.
    public mutating func remembered(always: Bool) -> InputAnswer? {
        always && canInject ? answer(.always) : nil
    }

    /// The receiver asks for control: the answer given before, a no if nothing can be injected, or a question.
    public mutating func request() -> Step {
        if !canInject { answered = .no }
        if let a = answered { return .reply(a) }
        if asking { return .wait }
        asking = true
        return .ask
    }

    /// The person answered: the gate opens (or stays shut).
    @discardableResult
    public mutating func answer(_ a: InputAnswer) -> InputAnswer {
        asking = false
        answered = a
        granted = a != .no && canInject
        return a
    }

    /// The `input` message for an answer.
    public static func message(_ a: InputAnswer, token: UInt64, area: (Int, Int)) -> [String: Any] {
        guard a != .no else { return ["t": "input", "grant": "no"] }
        return ["t": "input", "grant": a.rawValue, "token": String(format: "%016llx", token),
                "accept": InputInjector.accepts, "area": [area.0, area.1]]
    }
}

/// The datagrams of one picture's flow: read off the video socket on a thread of their own, tracked, injected.
public final class InputLink {
    public let token: UInt64
    private let socket: UdpSender
    private let injector: InputInjector
    private let lock = NSLock()
    private var isGranted = false
    private var stopped = false

    public init(socket: UdpSender, injector: InputInjector,
                token: UInt64 = UInt64.random(in: 1...UInt64.max)) {
        self.socket = socket
        self.injector = injector
        self.token = token
    }

    public var granted: Bool {
        get { lock.lock(); defer { lock.unlock() }; return isGranted }
        set { lock.lock(); isGranted = newValue; lock.unlock() }
    }

    private var isStopped: Bool {
        lock.lock(); defer { lock.unlock() }
        return stopped
    }

    public func start() {
        let t = Thread { [self] in run() }
        t.name = "luch.input"
        t.qualityOfService = .userInteractive
        t.start()
    }

    /// The thread ends within 100 ms and lets everything go.
    public func stop() {
        lock.lock()
        stopped = true
        lock.unlock()
    }

    private func run() {
        var tracker = InputTracker(token: token)
        var buf = [UInt8](repeating: 0, count: 2048)
        var lastTick = 0.0
        let now = { ProcessInfo.processInfo.systemUptime }
        while !isStopped {
            let n = socket.receive(into: &buf, timeoutMs: 100)
            if n < 0 { break }
            if n > 0 {
                let pkt = Array(buf[0..<n])
                // Another kind on this flow, or before the grant: dropped.
                if pkt.first == InputWire.kind, InputWire.token(of: pkt) == token, granted {
                    do {
                        tracker.apply(try InputWire.parse(pkt), now: now()).forEach(injector.inject)
                    } catch {
                        log("input: dropped a datagram (\(error))")
                    }
                }
            }
            if now() - lastTick >= 0.1 {
                lastTick = now()
                // Taken back with something held: let it go.
                (granted ? tracker.watchdog(now: lastTick) : tracker.releaseAll()).forEach(injector.inject)
            }
        }
        tracker.releaseAll().forEach(injector.inject)
    }
}

/// Whether macOS lets this app post events, and its request for that; a fake in the tests.
public struct InputPermission {
    public var allowed: () -> Bool
    public var request: () -> Void

    public init(allowed: @escaping () -> Bool, request: @escaping () -> Void) {
        self.allowed = allowed
        self.request = request
    }

    public static let system = InputPermission(allowed: { InputPoster.allowed },
                                               request: { InputPoster.requestAccess() })
}

/// The input part of an Engine's session: the gate across the screen's switches, a link per picture.
public final class InputControl {
    /// Asks the person "May <receiver> control this Mac?" and calls back once, on any thread.
    public typealias Ask = (_ receiver: String, _ devices: [String], _ reply: @escaping (InputAnswer) -> Void) -> Void

    private let always: Bool
    private let ask: Ask
    private let remember: (InputAnswer) -> Void
    private let permission: InputPermission
    /// How long to wait for the switch in Settings once macOS has asked for it.
    private let permissionWait: TimeInterval
    private let post: (MacInput) -> Void
    private let location: () -> CGPoint
    private let q = DispatchQueue(label: "luch.input.control")
    private var gate = InputGate(canInject: true)
    private var link: InputLink?
    private var area: (Int, Int) = (0, 0)
    private var send: ([String: Any]) -> Void = { _ in }
    private var receiverName = "The TV"
    /// The receiver said "input" in its welcome.
    private var receiverCan = false

    /// `always`: the person said so for this receiver before. `remember` keeps an "always".
    public init(always: Bool, ask: @escaping Ask, remember: @escaping (InputAnswer) -> Void,
                permission: InputPermission = .system, permissionWait: TimeInterval = 60,
                post: @escaping (MacInput) -> Void = InputPoster.post,
                location: @escaping () -> CGPoint = InputPoster.location) {
        self.always = always
        self.ask = ask
        self.remember = remember
        self.permission = permission
        self.permissionWait = permissionWait
        self.post = post
        self.location = location
    }

    /// Goes into the hello's `want`: a sender that can inject says so, or the receiver never asks.
    public static let want = "input"

    public var granted: Bool { q.sync { gate.granted } }

    /// The receiver's welcome: whether it captures any input at all.
    public func welcome(_ msg: [String: Any], name: String) {
        q.sync {
            receiverCan = !((msg["input"] as? [Any]) ?? []).isEmpty
            receiverName = name
        }
    }

    /// A picture started (`ready`): input comes back on its video flow. The grant of this session (a new token
    /// for the new flow), or an "always" from before, goes out at once.
    public func started(video: UdpSender, geometry: @escaping () -> InputGeometry,
                        send: @escaping ([String: Any]) -> Void) {
        q.sync {
            link?.stop()
            self.send = send
            guard receiverCan else { return }
            let injector = InputInjector(geometry: geometry, location: location, post: post,
                                         clock: { ProcessInfo.processInfo.systemUptime })
            let l = InputLink(socket: video, injector: injector)
            l.start()
            link = l
            area = geometry().areaSize
            if gate.answered == nil, permission.allowed() { _ = gate.remembered(always: always) }
            l.granted = gate.granted
            if let a = gate.answered, a != .no { send(InputGate.message(a, token: l.token, area: area)) }
        }
    }

    /// The picture ended (a switch of the screen, or the session).
    public func ended() {
        q.sync {
            link?.stop()
            link = nil
        }
    }

    /// The receiver's control messages; true for the ones that are input's.
    public func handle(_ msg: [String: Any]) -> Bool {
        guard msg["t"] as? String == "input_request" else { return false }
        q.async { [self] in
            guard let l = link else {
                send(["t": "input", "grant": "no"])
                return
            }
            switch gate.request() {
            case let .reply(a): send(InputGate.message(a, token: l.token, area: area))
            case .wait: break
            case .ask:
                let devices = (msg["devices"] as? [String]) ?? []
                ask(receiverName, devices) { [weak self] a in self?.answered(a) }
            }
        }
        return true
    }

    /// Control taken back during the session: the receiver stops sending, a new request is answered no.
    public func takeBack() {
        q.async { [self] in
            guard gate.granted else { return }
            gate.answer(.no)
            link?.granted = false
            send(["t": "input", "grant": "off"])
        }
    }

    private func answered(_ a: InputAnswer) {
        if a == .always { remember(.always) }
        // Off the asker's (main) thread: the wait below may be long.
        DispatchQueue.global(qos: .userInitiated).async { [self] in
            // Allowed by the person: macOS must allow it too. Its own request shows once; the switch in Settings is
            // waited for, the receiver meanwhile keeps its input to itself.
            if a != .no, !permission.allowed() {
                permission.request()
                let until = Date().addingTimeInterval(permissionWait)
                while !permission.allowed(), Date() < until { Thread.sleep(forTimeInterval: 0.25) }
                if !permission.allowed() { log("input: Accessibility not allowed for Luch, answering no") }
            }
            let final = a != .no && permission.allowed() ? a : .no
            q.async { [self] in
                gate.answer(final)
                link?.granted = gate.granted
                send(InputGate.message(final, token: link?.token ?? 0, area: area))
            }
        }
    }
}
