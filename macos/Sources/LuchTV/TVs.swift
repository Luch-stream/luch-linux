// The TVs found so far and what they play for us; the local API's commands for them (cast, media_cmd, media_state,
// stop_cast, volume), as luchd's D-Bus object does for its `tv::Tvs`.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchCast
import LuchProto

public final class TVs {
    private let lock = NSLock()
    private var found: [String: (device: Device, seen: Double)] = [:]
    /// What plays for each row (a row's id is its best way's id, whichever way plays).
    private var sessions: [String: TVSession] = [:]
    private var castNames: [String: String] = [:] // mDNS instance -> device id, for its removal
    private let browser = CastBrowser()
    private let wake = DispatchSemaphore(value: 0)
    private var started = false

    /// The list changed, or what plays on a TV did (the menu follows it). Called on any thread.
    public var onChange: (() -> Void)?
    /// Every media_state of a row, the last one `stopped`.
    public var onState: ((_ row: String, [String: Any]) -> Void)?

    /// A device not seen again for this long is gone (SSDP has no reliable bye; mDNS tells removals itself).
    static let forgetAfter = 200.0

    public init() {}

    /// Starts finding Chromecasts (mDNS, all the time) and DLNA renderers (a search at start, every minute and on
    /// `refresh`).
    public func start(ssdp target: SSDP.Target = SSDP.Target()) {
        lock.lock()
        defer { lock.unlock() }
        guard !started else { return }
        started = true
        browser.onFound = { [weak self] instance, d in
            guard let self else { return }
            self.lock.lock()
            self.castNames[instance] = d.id
            self.lock.unlock()
            if self.saw(d) { self.onChange?() }
        }
        browser.onGone = { [weak self] instance in
            guard let self else { return }
            self.lock.lock()
            let id = self.castNames.removeValue(forKey: instance)
            self.lock.unlock()
            if let id, self.remove(id) { self.onChange?() }
        }
        browser.start()
        Thread.detachNewThread { [weak self] in
            while let self {
                self.searchOnce(target)
                _ = self.wake.wait(timeout: .now() + 60)
            }
        }
    }

    /// Looks for renderers now.
    public func refresh() {
        wake.signal()
    }

    func searchOnce(_ target: SSDP.Target) {
        var changed = false
        do {
            for a in try SSDP.search(target, wait: 2) {
                switch SSDP.describe(a.location) {
                case .success(let d?): changed = saw(d) || changed
                case .success(nil): break
                case .failure(let e): log("tv: renderer \(a.location): \(e.message)")
                }
            }
        } catch {
            log("tv: SSDP search failed: \(error.localizedDescription)")
        }
        if prune(.dlna) || changed { onChange?() }
    }

    /// Notes a device; true when it is new or changed.
    @discardableResult
    public func saw(_ d: Device) -> Bool {
        lock.lock()
        let new = found[d.id]?.device != d
        found[d.id] = (d, now())
        lock.unlock()
        if new { log("tv: found \(d.kind.rawValue): \(d.name) (\(d.model)) at \(d.address)") }
        return new
    }

    @discardableResult
    public func remove(_ id: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return found.removeValue(forKey: id) != nil
    }

    /// Forgets devices of `kind` not seen for a while; true when any went.
    func prune(_ kind: TVKind) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        let before = found.count
        found = found.filter { $0.value.device.kind != kind || now() - $0.value.seen < Self.forgetAfter }
        return found.count != before
    }

    public func device(_ id: String) -> Device? {
        lock.lock()
        defer { lock.unlock() }
        return found[id]?.device
    }

    public var devices: [Device] {
        lock.lock()
        defer { lock.unlock() }
        return found.values.map(\.device)
    }

    /// Every TV with the ways to it, from the Luch receivers given and the TVs found.
    public func groups(luch: [Entry]) -> [[Entry]] {
        group(luch + devices.map(Entry.init))
    }

    /// The session playing for `row`, if one still runs.
    public func session(_ row: String) -> TVSession? {
        lock.lock()
        defer { lock.unlock() }
        if sessions[row]?.ended == true { sessions[row] = nil }
        return sessions[row]
    }

    /// `s` plays for `row` now; one that played there before leaves the TV to it.
    func start(_ row: String, _ s: TVSession) {
        lock.lock()
        let old = sessions.updateValue(s, forKey: row)
        lock.unlock()
        _ = old?.ask(.leave)
    }

    // MARK: - the local API

    /// What `cmd` does on `row` when a TV without Luch is meant (the row is one, `transport` names one, or one plays
    /// for it); nil when it is a Luch receiver's to do. `luch` are the Luch receivers found, for the rows.
    public func handle(_ request: [String: Any], row: String, luch: [Entry], files: () throws -> FileServer) -> [String: Any]? {
        let cmd = request["cmd"] as? String ?? ""
        let options = request["options"] as? [String: Any] ?? [:]
        let forced: TVKind?
        switch options["transport"] as? String {
        case nil, "", "auto": forced = nil
        case let t?:
            guard let k = TVKind(rawValue: t) else { return ["error": "transport must be luch, cast or dlna, not \(t)"] }
            forced = k
        }
        let groups = self.groups(luch: luch)
        let way = pick(groups, id: row, forced: forced)
        if forced != nil, way == nil {
            return ["error": "\(row) cannot be reached that way: its ways are " +
                    (groups.first { $0.contains { $0.id == row } }?.map(\.kind.rawValue).joined(separator: ", ") ?? "none")]
        }
        let playing = session(row)
        let isTV = way.map { $0.kind != .luch } ?? (TVKind.of(row) != .luch)
        do {
            switch cmd {
            case "cast":
                guard isTV else { return nil }
                guard let id = way?.id, let d = device(id) else { return ["error": "the TV went away"] }
                return ["ok": true, "kind": try cast(row: row, device: d, request: request, files: try files())]
            case "media_cmd":
                guard let s = playing else { return isTV ? ["error": "nothing plays there for us"] : nil }
                guard let c = request["media"] as? [String: Any], c["do"] is String else { return ["error": "no media command"] }
                _ = try s.ask(.cmd(c)).get()
                return ["ok": true]
            case "media_state":
                guard let s = playing else { return isTV ? ["ok": true, "state": [String: Any]()] : nil }
                return ["ok": true, "state": s.state]
            case "stop_cast":
                guard let s = playing else { return isTV ? ["ok": true] : nil }
                let r = s.ask(.stop)
                lock.lock()
                if sessions[row] === s { sessions[row] = nil }
                lock.unlock()
                (try? files())?.forget(client: s.device.address)
                onChange?()
                _ = try r.get()
                return ["ok": true]
            case "volume":
                guard let s = playing else {
                    return isTV ? ["error": "this TV's volume can be set while it plays something cast to it"] : nil
                }
                let what = request["what"] as? String ?? ""
                return ["ok": true, "volume": try s.ask(.volume(try Self.volumeMessage(what))).get()]
            case "receiver_modes":
                guard isTV, let k = way?.kind else { return nil }
                return ["ok": true, "modes": k.can.filter { $0 == "media" || $0 == "photo" }]
            case "sleep", "page":
                guard isTV else { return nil }
                return ["error": cmd == "sleep" ? "only a Luch receiver can be put to sleep from here"
                    : "this TV cannot show documents: only a Luch receiver can"]
            default:
                return nil
            }
        } catch {
            return ["error": error.localizedDescription]
        }
    }

    /// A `volume` word as Luch's message: "0".."100", "+", "-", "mute", "unmute"; "" only reads it.
    static func volumeMessage(_ what: String) throws -> [String: Any] {
        switch what {
        case "": return ["t": "volume", "step": 0]
        case "+", "up": return ["t": "volume", "step": 1]
        case "-", "down": return ["t": "volume", "step": -1]
        case "mute": return ["t": "volume", "muted": true]
        case "unmute": return ["t": "volume", "muted": false]
        default:
            guard let n = Int(what.replacingOccurrences(of: "%", with: "")), (0...100).contains(n) else {
                throw TVError("volume: 0..100, +, -, mute or unmute")
            }
            return ["t": "volume", "level": n]
        }
    }

    /// `cast` to a TV without Luch: the same message a Luch receiver would get, played through the TV's own way.
    func cast(row: String, device d: Device, request: [String: Any], files server: FileServer) throws -> String {
        let items = request["items"] as? [String] ?? []
        if items.contains(where: { Cast.kind($0) == .document }) {
            throw TVError("this TV cannot show documents: only a Luch receiver can")
        }
        var options = request["options"] as? [String: Any] ?? [:]
        options["transport"] = nil
        var o = try CastOptions(json: options)
        // Such a TV cannot send a page's headers: a link that needs them goes through this Mac.
        if !o.headers.isEmpty && o.via == nil { o.via = "sender" }
        guard let me = localAddress(for: d.address) else { throw TVError("no way to \(d.address) from here") }
        let msg = try Cast.build(items, o: o, server: server, client: d.address, me: me)
        let kind = msg["t"] as? String ?? ""
        var m = try Media(message: msg)
        switch d.kind {
        case .dlna: DLNA.prepare(server, m)
        case .cast: CastV2.prepare(server, &m)
        case .luch: break
        }
        let s = try play(d, m) { [weak self] s, v in
            guard let self else { return }
            // A session something newer replaced on this TV is no longer news, and its files stay for the new one.
            self.lock.lock()
            let replaced = self.sessions[row].map { $0 !== s } ?? false
            self.lock.unlock()
            if replaced { return }
            self.onState?(row, v)
            if v["state"] as? String == "stopped" {
                server.forget(client: d.address)
                self.onChange?()
            }
        }
        log("tv: \(d.name) plays \(kind) over \(d.kind.rawValue)")
        start(row, s)
        onChange?()
        return kind
    }
}
