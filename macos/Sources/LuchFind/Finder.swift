// What luchd's Finder and wake do for the Mac app: the discovery ladder over mDNS, the sweep, Bluetooth, sound and
// the camera; receivers added by hand; the receivers talked to before, and turning them on.
//
// Every receiver found any way is checked with the real hello before it is listed, and joins mDNS's in one list.
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreBluetooth
import Foundation
import LuchNet
import LuchProto

public final class Finder {
    public let known: KnownReceivers
    private let me: ProbeIdentity
    private let mdns: () -> [Receiver]
    private let clock: FindClock
    /// The list changed (a receiver found or gone); called on any thread.
    public var onChange: (() -> Void)?
    /// A FindProgress, for whoever follows it (the menu); called on any thread.
    public var onProgress: ((FindProgress) -> Void)?
    /// Whether a session with the receiver is open (it is then on, whatever a probe says).
    public var isConnected: (String) -> Bool = { _ in false }
    /// The ladder's last rung, connecting directly (Direct.swift sets it).
    var directRung: (() throws -> String)?

    private let lock = NSLock()
    /// Receivers found other ways than mDNS, by id.
    private var found: [String: Receiver] = [:]
    private var via: [String: Set<String>] = [:]
    private var rssi: [String: Int] = [:]
    private var welcomes: [String: [String: Any]] = [:]
    private var tokens: [String: String] = [:]
    /// Receivers heard over Bluetooth that offer a direct connection, by card id.
    private var nearby: [String: Heard] = [:]
    private var last = FindProgress()
    private var lastEnd: TimeInterval?

    /// The automatic ladder is climbed at most this often; mDNS keeps browsing anyway.
    static let autoEvery: TimeInterval = 30
    /// How long the learned advertisement is on the air, about one press of the remote (wake.rs, HOLD).
    public static let holdMs = 2000

    public init(me: ProbeIdentity, known: KnownReceivers = KnownReceivers(file: KnownReceivers.defaultFile()),
                clock: FindClock = WallClock(), mdns: @escaping () -> [Receiver]) {
        self.me = me
        self.known = known
        self.clock = clock
        self.mdns = mdns
    }

    // MARK: - the list

    /// mDNS's receivers and the ones found other ways, nearest first where Bluetooth heard them.
    public func list() -> [Receiver] {
        let fromMdns = mdns()
        lock.lock()
        let others = found.values.filter { r in !fromMdns.contains { $0.id == r.id } }
        let strength = rssi
        lock.unlock()
        return (fromMdns + others).enumerated().sorted { a, b in
            let (ra, rb) = (strength[a.element.id] ?? Int.min, strength[b.element.id] ?? Int.min)
            return ra != rb ? ra > rb : a.offset < b.offset
        }.map(\.element)
    }

    /// How each listed receiver was found, and its Bluetooth signal: {"<id>":{"via":["mdns","ble"],"rssi":-48}}.
    public func discovered() -> [String: [String: Any]] {
        let fromMdns = Set(mdns().map(\.id))
        lock.lock()
        defer { lock.unlock() }
        var out: [String: [String: Any]] = [:]
        for id in fromMdns.union(found.keys) {
            var ways = via[id] ?? []
            if fromMdns.contains(id) { ways.insert("mdns") }
            var d: [String: Any] = ["via": ways.sorted()]
            if let r = rssi[id] { d["rssi"] = r }
            out[id] = d
        }
        return out
    }

    /// The pairing token a QR code or the TV's sound gave for a receiver, for its next hello (PROTOCOL.md 1e).
    public func token(for id: String) -> String? {
        lock.lock()
        defer { lock.unlock() }
        return tokens[id]
    }

    /// The same, used up: a token is good for one hello (the next connection's).
    public func takeToken(for id: String) -> String? {
        lock.lock()
        defer { lock.unlock() }
        return tokens.removeValue(forKey: id)
    }

    func add(_ r: Receiver, via how: String, rssi strength: Int?, token: String?, welcome: [String: Any]) {
        lock.lock()
        let fresh = found[r.id] == nil
        found[r.id] = r
        via[r.id, default: []].insert(how)
        if let strength { rssi[r.id] = strength }
        if let token { tokens[r.id] = token }
        welcomes[r.id] = welcome
        lock.unlock()
        log("find: \(r.name) at \(r.address):\(r.port) via \(how)\(fresh ? "" : " (again)")")
        onChange?()
    }

    // MARK: - the ladder

    /// The last progress (luchd's FindStatus).
    public func status() -> [String: Any] {
        lock.lock()
        defer { lock.unlock() }
        return last.json
    }

    /// Starts a climb unless one is running (or, for "auto", one ended a moment ago).
    public func start(_ mode: String) throws {
        let ladder = try Ladder(mode: mode)
        lock.lock()
        if last.running || (["", "auto"].contains(mode) && lastEnd.map { clock.now - $0 < Finder.autoEvery } == true) {
            lock.unlock()
            return
        }
        last = FindProgress()
        last.running = true
        last.mode = mode
        lock.unlock()
        Thread.detachNewThread { [self] in
            prune()
            let p = climb(ladder, mode: mode, clock: clock, found: { self.list().count }, run: { rung, done in
                Thread.detachNewThread { done(Result { try self.run(rung, asked: !["", "auto"].contains(mode)) }.mapError { $0 as? FindError ?? FindError("\($0)") }) }
            }, report: { p in
                log("find: \(p.json)")
                self.lock.lock()
                self.last = p
                self.lock.unlock()
                self.onProgress?(p)
            })
            lock.lock()
            last = p
            lastEnd = clock.now
            lock.unlock()
        }
    }

    /// Receivers found other ways than mDNS go when they no longer answer (mDNS has its own removal).
    private func prune() {
        lock.lock()
        let quiet = found.values.filter { !isConnected($0.id) }
        lock.unlock()
        var gone = false
        for r in quiet where !answers(r.address, r.port) {
            lock.lock()
            found.removeValue(forKey: r.id)
            via.removeValue(forKey: r.id)
            rssi.removeValue(forKey: r.id)
            lock.unlock()
            gone = true
        }
        if gone { onChange?() }
    }

    /// `asked`: the person chose to look this way; the automatic climb never brings up a permission dialog.
    private func run(_ rung: Rung, asked: Bool) throws -> String {
        switch rung {
        case .mdns:
            Thread.sleep(forTimeInterval: Ladder.firstPatience - 0.5)
            return "\(mdns().count) found"
        case .scan:
            let lans = netInterfaces().filter(scannable)
            guard !lans.isEmpty else { throw FindError("no LAN interface to scan (a VPN or tunnel is never scanned)") }
            let arp = arpTable()
            var targets: [(UInt32, NetInterface)] = []
            var nets: [String] = []
            for i in lans {
                let t = Sweep.plan(i, arp: arp)
                nets.append("\(t.count) addresses on \(i.name)")
                targets += t.map { ($0, i) }
            }
            let names = Sweep.run(targets, probe: { [me] ip, i -> (Receiver, [String: Any])? in
                guard let w = probe(ip, Proto.controlPort, ifindex: i.index, me: me),
                      let r = receiverFrom(welcome: w, ip: ip, port: Proto.controlPort, ifname: i.name, ifindex: i.index)
                else { return nil }
                return (r, w)
            }, found: { [self] hit in add(hit.0, via: "scan", rssi: nil, token: nil, welcome: hit.1) }).map(\.0.name)
            return "asked \(nets.joined(separator: ", ")): " + (names.isEmpty ? "no receiver" : "found \(names.joined(separator: ", "))")
        case .direct:
            guard let directRung else { throw FindError("connecting directly is not available here") }
            return try directRung()
        case .ble:
            if !asked, CBCentralManager.authorization == .notDetermined {
                throw FindError("Bluetooth is not allowed yet: Find Receivers, Listen over Bluetooth (or `luchctl find ble`) asks for it")
            }
            var notes: [String] = []
            let tries = DispatchGroup()
            let noteLock = NSLock()
            // Each receiver is tried as soon as it is heard, while the scan goes on.
            let heard = try BLE.search(seconds: Ladder.bleListen) { [self] h in
                noteNearby(h)
                tries.enter()
                Thread.detachNewThread { [self] in
                    var note: String
                    do {
                        note = try reach(h.card, via: "ble", rssi: h.rssi)
                    } catch {
                        note = h.card.addr == nil ? "heard \(h.card.id), not on this network"
                                                  : "heard \(h.card.id) but \(error.localizedDescription)"
                        if h.card.psm != 0 { note += "; direct possible (`luchctl direct \(h.card.id)`)" }
                    }
                    noteLock.lock()
                    notes.append(note)
                    noteLock.unlock()
                    tries.leave()
                }
            }
            _ = tries.wait(timeout: .now() + 5)
            // The signal strength of the whole scan, for the order of the list.
            lock.lock()
            for h in heard {
                if let id = found.keys.first(where: { $0.replacingOccurrences(of: "-", with: "").lowercased().hasPrefix(h.card.id) }),
                   let s = h.rssi {
                    rssi[id] = s
                }
            }
            lock.unlock()
            onChange?()
            noteLock.lock()
            defer { noteLock.unlock() }
            return heard.isEmpty ? "no receiver heard" : notes.joined(separator: "; ")
        case .sound:
            let cards = try Microphone.listen(seconds: Ladder.personListen).compactMap(Card.decode)
            let notes = cards.map { c in (try? reach(c, via: "sound", rssi: nil)) ?? "heard \(c.id) but it does not answer" }
            return notes.isEmpty ? "heard nothing" : notes.joined(separator: "; ")
        case .qr:
            let notes = try QR.watch(seconds: Ladder.personListen).map { c in
                do { return try reach(c, via: "qr", rssi: nil) } catch { return error.localizedDescription }
            }
            return notes.isEmpty ? "saw no code" : notes.joined(separator: "; ")
        }
    }

    /// Connects to the card's address from the right interface, checks it is a receiver (and the one the card
    /// names), and lists it. Returns its name.
    @discardableResult
    func reach(_ card: Card, via how: String, rssi strength: Int?) throws -> String {
        guard let (ip, port) = card.addr else { throw FindError("no address") }
        guard let iface = interfaceFor(ip) else { throw FindError("no network interface reaches \(ipString(ip))") }
        guard let w = probe(ip, port, ifindex: iface.index, me: me, token: card.token, connectTimeout: 2) else {
            throw FindError("\(ipString(ip)):\(port) does not answer here: the network may keep devices apart")
        }
        guard let r = receiverFrom(welcome: w, ip: ip, port: port, ifname: iface.name, ifindex: iface.index) else {
            throw FindError("\(ipString(ip)):\(port) is not a Luch receiver")
        }
        guard r.id.replacingOccurrences(of: "-", with: "").lowercased().hasPrefix(card.id) else {
            throw FindError("\(ipString(ip)):\(port) is another receiver (\(r.name)) than the one heard")
        }
        // A token the receiver no longer takes (it was used, or the TV restarted): listed all the same, unpaired.
        let tokenOK = w["token"] as? String == "ok"
        add(r, via: how, rssi: strength, token: tokenOK ? card.token : nil, welcome: w)
        return card.token != nil && !tokenOK ? r.name + " (its code has changed since: pairing needs a fresh one)" : r.name
    }

    /// Lists a receiver from what a person has: its QR link or its address (host[:port]). Returns its id.
    public func add(link: String) throws -> String {
        guard let card = Card.parseLink(link) else { throw FindError("not a receiver's link or address: \(link)") }
        try reach(card, via: link.trimmingCharacters(in: .whitespaces).hasPrefix("luch://") ? "qr" : "manual", rssi: nil)
        let ip = ipString(card.addr!.ip)
        lock.lock()
        defer { lock.unlock() }
        guard let r = found.values.first(where: {
            $0.address == ip && (card.id.isEmpty || $0.id.replacingOccurrences(of: "-", with: "").lowercased().hasPrefix(card.id))
        }) else { throw FindError("receiver went away") }
        return r.id
    }

    // MARK: - heard over Bluetooth, for a direct connection

    /// Keeps a receiver heard over the air that offers a direct connection.
    func noteNearby(_ h: Heard) {
        guard h.card.psm != 0 else { return }
        lock.lock()
        nearby[h.card.id] = h
        lock.unlock()
    }

    /// The receivers heard offering a direct connection, nearest first.
    public func directNearby() -> [Heard] {
        lock.lock()
        defer { lock.unlock() }
        return nearestFirst(Array(nearby.values))
    }

    /// The listed receiver whose id the card's 8 hex digits start, if any.
    public func listed(card id: String) -> Receiver? {
        list().first { $0.id.replacingOccurrences(of: "-", with: "").lowercased().hasPrefix(id) }
    }

    /// Drops a receiver found other ways than mDNS (a direct one, once its network is left).
    func drop(_ id: String) {
        lock.lock()
        let was = found.removeValue(forKey: id) != nil
        via.removeValue(forKey: id)
        rssi.removeValue(forKey: id)
        lock.unlock()
        if was { onChange?() }
    }

    // MARK: - what a receiver is

    /// luchd's Info: `android` ("Android 5.1", empty when unknown), `limits` in plain words, `audio_latency_ms`.
    public func info(_ r: Receiver) -> [String: Any] {
        lock.lock()
        var w = welcomes[r.id]
        lock.unlock()
        if w == nil, let ip = parseIPv4(r.address) {
            w = probe(ip, r.port, ifindex: r.interfaceIndex, me: me, connectTimeout: 2)
            lock.lock()
            welcomes[r.id] = w
            lock.unlock()
        }
        return Finder.info(welcome: w ?? [:])
    }

    public static func info(welcome w: [String: Any]) -> [String: Any] {
        let limits = w["limits"] as? [String: Any] ?? [:]
        let lag = (limits["audio_latency_ms"] as? NSNumber)?.intValue
        var out: [String: Any] = [
            "android": (w["sdk"] as? NSNumber).map { android($0.intValue) } ?? "",
            "limits": (limits["limits"] as? [String] ?? []).map { describe($0, lag: lag ?? 0) },
            "audio": "auto",
        ]
        if let lag { out["audio_latency_ms"] = lag }
        return out
    }

    /// "Android 5.1" from the API level, for people.
    static func android(_ sdk: Int) -> String {
        let names = [21: "5.0", 22: "5.1", 23: "6.0", 24: "7.0", 25: "7.1", 26: "8.0", 27: "8.1", 28: "9", 29: "10",
                     30: "11", 31: "12", 32: "12L", 33: "13", 34: "14", 35: "15", 36: "16"]
        return names[sdk].map { "Android \($0)" } ?? "API \(sdk)"
    }

    /// One limit in plain words (luchd's limits.rs); unknown ones as they are.
    static func describe(_ limit: String, lag: Int) -> String {
        switch limit {
        case "audio_lag":
            return "sound trails the picture by about \(lag) ms (Android 5 and 6 cannot shorten it)"
        case "decoder_low_latency": return "no low-latency decoder mode (Android 11+)"
        case "game_mode": return "the display is not asked for its game mode (Android 11+)"
        case "wifi_low_latency": return "no low-latency Wi-Fi mode (Android 10+)"
        case "hdr_info": return "HDR support unknown (Android 7+)"
        case "display_modes": return "only the current display mode is known (Android 6+)"
        default: return limit
        }
    }

    // MARK: - receivers talked to before

    /// Notes a receiver this Mac has just connected to, with its MAC for Wake-on-LAN.
    public func remember(_ r: Receiver) {
        let mac = arpMac(arpTable(), r.address)
        known.remember(id: r.id, name: r.name, model: r.model, address: r.address, mac: mac)
    }

    /// Every known receiver as luchd's Known gives it: its fields and `id`, `online` (on the LAN now), `wake` (the
    /// ways it can be turned on) and `hint`.
    public func knownRows() -> [[String: Any]] {
        let listed = Dictionary(list().map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        let all = known.load().sorted { ($0.value["name"] as? String ?? "") < ($1.value["name"] as? String ?? "") }
        var online = [Bool](repeating: false, count: all.count)
        let onlineLock = NSLock()
        // mDNS keeps a receiver for its record's TTL after it went to standby: one not connected must answer now.
        DispatchQueue.concurrentPerform(iterations: all.count) { i in
            guard let r = listed[all[i].key], isConnected(r.id) || answers(r.address, r.port) else { return }
            onlineLock.lock()
            online[i] = true
            onlineLock.unlock()
        }
        return all.enumerated().map { i, kv in
            var r = kv.value
            r["id"] = kv.key
            r["online"] = online[i]
            r["wake"] = KnownReceivers.methods(r)
            if let h = KnownReceivers.hint(r) { r["hint"] = h }
            return r
        }
    }

    /// Turns a known receiver on every way it can be: Wake-on-LAN, and the remote's Bluetooth advertisement sent by
    /// another Luch device that is on. Returns the ways tried ("wol", "ble").
    public func wake(_ id: String) throws -> [String] {
        guard let r = known.load()[id] else { throw FindError("unknown receiver: \(id)") }
        var tried: [String] = []
        var errors: [String] = []
        if let mac = (r["mac"] as? String).flatMap(parseMac) {
            do {
                try sendWakeOnLan(mac: mac, lastAddress: r["address"] as? String)
                tried.append("wol")
            } catch {
                errors.append("Wake-on-LAN: \(error.localizedDescription)")
            }
        }
        if let adv = (r["ble"] as? String).flatMap(parseHex) {
            do {
                let relays = list().filter { $0.id != id }
                let by = try WakeRelay.ask(relays, data: adv, holdMs: Finder.holdMs, me: me)
                log("wake: \(by) sent the remote's signal")
                tried.append("ble")
            } catch {
                errors.append("Bluetooth: this Mac cannot send the remote's signal itself (macOS lets no app advertise it), and \(error.localizedDescription)")
            }
        }
        guard !tried.isEmpty else {
            throw FindError(errors.isEmpty
                ? KnownReceivers.hint(r) ?? "no way to wake it known: connect to it once while it is on (for its MAC), or `luchctl wake <r> learn`"
                : errors.joined(separator: "; "))
        }
        errors.forEach { log("wake: \($0)") }
        return tried
    }

    /// Listens up to `seconds` for the remote's power key and keeps it for waking `id`; returns it as hex.
    public func learn(_ id: String, seconds: TimeInterval) throws -> String {
        guard known.load()[id] != nil else { throw FindError("unknown receiver \(id): connect to it once first") }
        let hex = hexString(try BLE.learn(seconds: min(120, max(5, seconds))))
        try known.set(id: id, kind: "ble", value: hex)
        return hex
    }
}
