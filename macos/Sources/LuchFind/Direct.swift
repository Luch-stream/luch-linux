// Connecting directly: the last way to a receiver when this Mac and the TV cannot reach each other over a network
// (a hotel's Wi-Fi that keeps devices apart, no common LAN). Over Bluetooth LE the receiver is asked, with its own
// TLS control protocol (pinning, pairing, the person's Allow on the TV), for a Wi-Fi Direct group; this Mac joins it
// as an ordinary Wi-Fi network and reaches the receiver there like any other. It takes the Mac off its Wi-Fi, so it
// is never done without asking, and the Mac goes back to its network after.
//
//   Bluetooth: card with a PSM -> L2CAP channel -> StreamBridge (127.0.0.1) -> Connector (TLS, pin, pairing)
//     -> hello "via":"ble", "want":["direct"] -> {"t":"direct"} -> the group's SSID, passphrase, address
//   Wi-Fi: join it (DirectWiFi) -> the receiver at addr:tls_port, listed as found "direct" -> mirror, cast as usual
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchNet
import LuchProto

public final class Direct {
    /// TLS over Bluetooth is slower than over Wi-Fi.
    static let connectTimeout: TimeInterval = 15
    /// The person on the TV allows it, then the group comes up.
    static let allowWait: TimeInterval = 120
    /// For receivers to be heard when none was before.
    static let listenFor: TimeInterval = 8

    private let finder: Finder
    private let me: ProbeIdentity
    private let connector: Connector
    private let channels: DirectChannelOpener
    private let client: WiFiClient
    public let wifi: DirectWiFi
    private let lock = NSLock()
    private var state: [String: Any] = ["state": "idle", "note": ""]
    private var watching = false

    /// Asks the person the question (true: go ahead). Called on a background thread; no by default.
    public var confirm: (String) -> Bool = { _ in false }
    /// Whether a session with the receiver is open (a mirror or a cast); together with the finder's.
    public var isBusy: (String) -> Bool = { _ in false }
    /// What it does changed, or it went back to the network it left; called on any thread.
    public var onChange: (() -> Void)?
    /// It went back by itself (showing ended), with what it says about it; called on the watching thread.
    public var onLeft: ((String) -> Void)?
    /// Hears receivers for `seconds` (tests put in a fake).
    var listen: (TimeInterval) throws -> [Heard] = { try BLE.search(seconds: $0) { _ in } }
    /// The receiver at the group's address once joined, and its welcome (tests put in a fake).
    var reach: (DirectAnswer, _ id: String, _ name: String, _ ifname: String?) throws -> (Receiver, [String: Any])
    /// How often it looks whether showing has ended; nil: never by itself (tests call `check`).
    var watchEvery: TimeInterval? = 2

    public init(finder: Finder, me: ProbeIdentity, connector: Connector = .shared, wifi: WiFiClient = CoreWiFi(),
                channels: DirectChannelOpener = BLEChannels(),
                now: @escaping () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }) {
        self.finder = finder
        self.me = me
        self.connector = connector
        self.channels = channels
        client = wifi
        self.wifi = DirectWiFi(wifi: wifi, now: now)
        reach = { a, id, name, ifname in try Direct.reachInGroup(a, id: id, name: name, ifname: ifname, me: me) }
        finder.directRung = { [weak self] in
            guard let self else { throw FindError("connecting directly is not available here") }
            return try self.rung()
        }
    }

    // MARK: - what it is doing

    /// {"state": "idle"|"bluetooth"|"pairing"|"asking"|"joining"|"joined"|"failed", "note", "id" (while pairing),
    /// "ssid", "previous", "receiver" (while joined), "nearby": [{"id", "name", "rssi", "listed"}]}.
    public func status() -> [String: Any] {
        lock.lock()
        var s = state
        lock.unlock()
        if let j = wifi.joined {
            s["ssid"] = j.ssid
            s["receiver"] = j.receiverID
            s["name"] = j.name
            if let p = j.previous { s["previous"] = p }
        }
        s["nearby"] = finder.directNearby().map { h -> [String: Any] in
            var n: [String: Any] = ["id": h.card.id, "name": name(h.card.id), "listed": finder.listed(card: h.card.id) != nil]
            if let rssi = h.rssi { n["rssi"] = rssi }
            return n
        }
        return s
    }

    private func set(_ step: String, _ note: String, id: String? = nil) {
        lock.lock()
        state = ["state": step, "note": note]
        if let id { state["id"] = id }
        lock.unlock()
        log("direct: \(step): \(note)")
        onChange?()
    }

    /// Starts an attempt unless one is under way.
    private func begin() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard ["idle", "joined", "failed"].contains(state["state"] as? String ?? "") else { return false }
        state = ["state": "bluetooth", "note": ""]
        return true
    }

    // MARK: - which receiver

    /// The person's name for a receiver known only by its card: listed, paired or talked to before.
    func name(_ cardID: String) -> String {
        if let r = finder.listed(card: cardID) { return r.name }
        let paired = connector.paired().first { Direct.starts($0["id"] as? String, cardID) }
        if let n = paired?["name"] as? String { return n }
        let known = finder.known.load().first { Direct.starts($0.key, cardID) }
        return known?.value["name"] as? String ?? "receiver \(cardID)"
    }

    /// The whole id of a receiver known only by its card, when this Mac has seen it before.
    func fullID(_ cardID: String) -> String? {
        if let r = finder.listed(card: cardID) { return r.id }
        if let id = connector.paired().compactMap({ $0["id"] as? String }).first(where: { Direct.starts($0, cardID) }) {
            return id
        }
        return finder.known.load().keys.first { Direct.starts($0, cardID) }
    }

    static func starts(_ id: String?, _ cardID: String) -> Bool {
        id?.replacingOccurrences(of: "-", with: "").lowercased().hasPrefix(cardID) ?? false
    }

    /// The receiver `key` names (its card id, the start of its id, or its name), or the nearest one; heard again
    /// when none has been heard yet.
    func pick(_ key: String?) throws -> Heard {
        var heard = finder.directNearby()
        if heard.isEmpty {
            set("bluetooth", "listening for receivers over Bluetooth")
            try listen(Direct.listenFor).forEach(finder.noteNearby)
            heard = finder.directNearby()
        }
        guard !heard.isEmpty else {
            throw FindError("no receiver nearby offers a direct connection (Luch on the TV, Bluetooth on here and there?)")
        }
        guard let key, !key.isEmpty else { return heard[0] }
        let k = key.replacingOccurrences(of: "-", with: "").lowercased()
        let hit = heard.first { $0.card.id.hasPrefix(k) || k.hasPrefix($0.card.id) }
            ?? heard.first { name($0.card.id).caseInsensitiveCompare(key) == .orderedSame }
        guard let hit else {
            throw FindError("no receiver '\(key)' heard offering a direct connection; heard: "
                + heard.map { "\(name($0.card.id)) (\($0.card.id))" }.joined(separator: ", "))
        }
        return hit
    }

    /// The receiver to connect to and the question to ask about it. Asks for Location first (the network's name).
    public func plan(_ key: String?) throws -> (id: String, name: String, question: String) {
        let p = try planned(key)
        return (p.heard.card.id, p.name, p.question)
    }

    private func planned(_ key: String?) throws -> (heard: Heard, name: String, question: String) {
        let h = try pick(key)
        _ = client.allowNames()
        let tv = name(h.card.id)
        return (h, tv, wifi.question(tv: tv))
    }

    // MARK: - connecting

    /// The ladder's rung: the nearest receiver heard that is not on this network, after asking.
    func rung() throws -> String {
        if finder.directNearby().isEmpty { try listen(Direct.listenFor).forEach(finder.noteNearby) }
        let heard = finder.directNearby()
        guard let h = heard.first(where: { finder.listed(card: $0.card.id) == nil }) else {
            throw FindError(heard.isEmpty ? "no receiver nearby offers a direct connection"
                                          : "every receiver heard nearby is on this network already")
        }
        return try connect(h.card.id, confirmed: false)
    }

    /// Connects directly to the receiver `key` names (nil: the nearest), asking first unless `confirmed` (the person
    /// already said yes to `plan`'s question). Blocks until joined; returns what to tell the person.
    @discardableResult
    public func connect(_ key: String?, confirmed: Bool) throws -> String {
        guard begin() else { throw FindError("already connecting directly: `luchctl direct` follows it") }
        do {
            let (h, tv, question) = try planned(key)
            guard confirmed || confirm(question) else {
                set("idle", "not connected directly: this Mac stays on its network")
                throw FindError("not connected directly: this Mac stays on its network")
            }
            let asked = try ask(h, name: tv)
            defer { asked.done() }
            set("joining", "joining '\(asked.answer.ssid)'")
            try wifi.join(asked.answer, receiverID: asked.id, name: asked.name)
            let found: (Receiver, [String: Any])
            do {
                found = try reach(asked.answer, asked.id, asked.name, client.interfaceName)
            } catch {
                wifi.restore("\(asked.name) did not answer there")
                throw error
            }
            finder.add(found.0, via: "direct", rssi: h.rssi, token: nil, welcome: found.1)
            let back = wifi.joined?.previous.map { "'\($0)'" } ?? "its Wi-Fi network"
            let words = "joined '\(asked.answer.ssid)': \(found.0.name) is at \(found.0.address); mirror or cast to it "
                + "as usual. This Mac goes back to \(back) after"
            set("joined", words)
            watch()
            return words
        } catch {
            lock.lock()
            let declined = state["state"] as? String == "idle"
            lock.unlock()
            if !declined { set("failed", error.localizedDescription) }
            throw error
        }
    }

    /// Over Bluetooth: the TLS control connection (pairing first when it asks), then the request. The channel stays
    /// open until `done`, after joining.
    private func ask(_ h: Heard, name tv: String) throws
        -> (answer: DirectAnswer, id: String, name: String, done: () -> Void) {
        var id = fullID(h.card.id) ?? h.card.id
        var tv = tv
        let hello: [String: Any] = ["t": "hello", "v": Int(Proto.version), "id": me.id, "name": me.name,
                                    "app": Proto.app, "want": ["direct"], "via": "ble"]
        for attempt in 0..<2 {
            set("bluetooth", "connecting to \(tv) over Bluetooth")
            let channel = try channels.open(h, timeout: 20)
            let bridge: StreamBridge
            do {
                bridge = try StreamBridge(input: channel.input, output: channel.output)
            } catch {
                channel.close()
                throw error
            }
            let close = {
                bridge.close()
                channel.close()
            }
            let r = Receiver(id: id, name: tv, model: "", address: "127.0.0.1", interfaceName: "bluetooth",
                             interfaceIndex: 0, port: 0, service: "ble", tlsPort: bridge.port)
            let conn: Connection
            do {
                conn = try connector.open(r, hello: hello, timeout: Direct.connectTimeout)
            } catch ConnectError.pairingNeeded(let pairID, _, _) where attempt == 0 {
                // The code goes through the app's window or `luchctl pair`; the pairing runs on this channel.
                set("pairing", "\(tv) asks for pairing: enter the code it shows", id: pairID)
                defer { close() }
                try waitPaired(pairID, tv: tv)
                continue
            } catch {
                close()
                throw error
            }
            let ctl = conn.channel
            // Paired before its whole id was known: the pin goes where later connections look.
            if let whole = conn.welcome["id"] as? String, !whole.isEmpty, whole != id {
                do { try connector.repin(from: id, to: whole) } catch { log("direct: keeping the pairing: \(error.localizedDescription)") }
                id = whole
            }
            tv = conn.welcome["name"] as? String ?? tv
            set("asking", "asking \(tv): allow it on the TV")
            let offered = (conn.welcome["modes"] as? [String])?.contains("direct") ?? true
            do {
                let answer = try DirectAnswer.ask(ctl, tv: tv, offered: offered, timeout: Direct.allowWait)
                return (answer, id, tv, {
                    ctl.bye()
                    close()
                })
            } catch {
                ctl.bye()
                close()
                throw error
            }
        }
        throw FindError("\(tv) asked for pairing again")
    }

    private func waitPaired(_ id: String, tv: String) throws {
        let until = Date().addingTimeInterval(135)
        while Date() < until {
            let st = connector.pairing(id)
            switch st["step"] as? String {
            case "paired": return
            case "failed": throw FindError(st["reason"] as? String ?? "pairing with \(tv) failed")
            default: Thread.sleep(forTimeInterval: 0.3)
            }
        }
        throw FindError("no code was entered for \(tv) in time")
    }

    /// The receiver in its group: once this Mac has an address there (from the group owner's DHCP), its plain port
    /// answers the probe as on any LAN.
    static func reachInGroup(_ a: DirectAnswer, id: String, name: String, ifname: String?,
                             me: ProbeIdentity) throws -> (Receiver, [String: Any]) {
        guard let ip = parseIPv4(a.addr) else { throw FindError("\(name) gave no usable address") }
        let until = Date().addingTimeInterval(20)
        var lan: NetInterface?
        while lan == nil {
            lan = netInterfaces().first { $0.holds(ip) && !$0.isLoopback && (ifname == nil || $0.name == ifname) }
            if lan != nil { break }
            guard Date() < until else { throw FindError("joined '\(a.ssid)' but got no address there") }
            Thread.sleep(forTimeInterval: 0.5)
        }
        guard let lan, let w = probe(ip, a.port, ifindex: lan.index, me: me, connectTimeout: 3),
              var r = receiverFrom(welcome: w, ip: ip, port: a.port, ifname: lan.name, ifindex: lan.index) else {
            throw FindError("\(name) does not answer at \(a.addr) on '\(a.ssid)'")
        }
        guard r.id == id || starts(r.id, id) else { throw FindError("another receiver (\(r.name)) answers on '\(a.ssid)'") }
        if r.tlsPort == 0 { r.tlsPort = a.tlsPort }
        return (r, w)
    }

    // MARK: - going back

    private func watch() {
        guard let every = watchEvery else { return }
        lock.lock()
        let already = watching
        watching = true
        lock.unlock()
        if already { return }
        Thread.detachNewThread { [weak self] in
            while true {
                Thread.sleep(forTimeInterval: every)
                guard let self else { return }
                if !self.check() {
                    self.lock.lock()
                    self.watching = false
                    self.lock.unlock()
                    return
                }
            }
        }
    }

    /// One look whether showing has ended; false once the Mac is no longer on a direct network.
    @discardableResult
    func check() -> Bool {
        guard let j = wifi.joined else { return false }
        guard let words = wifi.tick(busy: isBusy(j.receiverID) || finder.isConnected(j.receiverID)) else { return true }
        left(j, words)
        onLeft?(words)
        return false
    }

    /// `luchctl direct off`: leaves the direct network now and goes back.
    @discardableResult
    public func off() throws -> String {
        let j = wifi.joined
        let words = try wifi.off()
        if let j { left(j, words) }
        return words
    }

    private func left(_ j: DirectWiFi.Joined, _ words: String) {
        finder.drop(j.receiverID)
        set("idle", words)
    }
}
