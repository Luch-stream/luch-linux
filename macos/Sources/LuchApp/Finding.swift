// The app's side of finding and waking receivers: the local API's find, add, refresh, id, known, wake and info
// commands (luchctl), and the menu's "Find Receivers", "Add Receiver…", "Search Harder" for an empty list, and the TVs
// that are off, with Turn On.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import LuchEngine
import LuchFind
import LuchNet

final class Finding {
    /// Answered on the local socket's thread: several wait for the network, Bluetooth or the person.
    static let commands = Set(["find", "find_status", "discovered", "add", "refresh", "sender_id", "known",
                                        "wake", "learn_wake", "set_wake", "info"]).union(DirectUI.commands)

    let finder: Finder
    /// Connecting directly over Bluetooth and Wi-Fi Direct (DirectMenu.swift).
    let direct: DirectUI
    /// The list changed, or what is being tried did; called on any thread.
    var onChange: (() -> Void)? {
        didSet {
            finder.onChange = onChange
            finder.onProgress = { [weak self] _ in self?.onChange?() }
            direct.onChange = onChange
        }
    }

    init(discovery: Discovery) {
        let me = senderIdentity()
        finder = Finder(me: ProbeIdentity(id: me.id, name: me.name)) { discovery.all() }
        direct = DirectUI(finder: finder, me: ProbeIdentity(id: me.id, name: me.name))
        // A QR code's or the TV's sound's token goes into the next hello to that receiver (PROTOCOL.md 1e).
        Connector.shared.takeToken = { [finder] id in finder.takeToken(for: id) }
    }

    // MARK: - local API

    func handle(_ request: [String: Any], receivers: [Receiver]) -> [String: Any] {
        let id = request["id"] as? String ?? ""
        if DirectUI.commands.contains(request["cmd"] as? String ?? "") { return direct.handle(request) }
        do {
            switch request["cmd"] as? String ?? "" {
            case "find":
                try finder.start(request["mode"] as? String ?? "all")
                return ["ok": true]
            case "find_status": return ["ok": true, "status": finder.status()]
            case "discovered": return ["ok": true, "discovered": finder.discovered()]
            case "add": return ["ok": true, "id": try finder.add(link: request["link"] as? String ?? "")]
            case "refresh":
                try finder.start("auto")
                return ["ok": true]
            case "sender_id": return ["ok": true, "id": senderIdentity().id]
            case "known": return ["ok": true, "known": finder.knownRows()]
            case "wake": return ["ok": true, "how": try finder.wake(id).joined(separator: ",")]
            case "learn_wake":
                let seconds = (request["seconds"] as? NSNumber)?.doubleValue ?? 30
                return ["ok": true, "ble": try finder.learn(id, seconds: seconds)]
            case "set_wake":
                try finder.known.set(id: id, kind: request["kind"] as? String ?? "", value: request["value"] as? String ?? "")
                return ["ok": true]
            case "info":
                let hits = id.isEmpty ? receivers : receivers.filter { $0.id == id || $0.name.caseInsensitiveCompare(id) == .orderedSame }
                guard hits.count == 1 else {
                    return ["error": hits.isEmpty ? "no receiver '\(id)' (see `luchctl list`)"
                                                  : "several receivers, name one: " + hits.map(\.name).joined(separator: ", ")]
                }
                // luchd's `paired` and `encrypted`: a paired receiver is reached over TLS only, never otherwise.
                var info = finder.info(hits[0])
                let paired = Connector.shared.isPaired(hits[0].id)
                info["paired"] = paired
                info["encrypted"] = paired && hits[0].tlsPort > 0
                info["tls"] = hits[0].tlsPort > 0
                return ["ok": true, "info": info]
            default:
                return ["error": "unknown command"]
            }
        } catch {
            return ["error": error.localizedDescription]
        }
    }

    // MARK: - menu

    /// The TVs talked to before that are not on now (greyed, with Wake), then Find Receivers and Add Receiver.
    func addMenu(to menu: NSMenu, online: [Receiver]) {
        let here = Set(online.map(\.id))
        let asleep = finder.known.load().filter { !here.contains($0.key) }
            .sorted { ($0.value["name"] as? String ?? "") < ($1.value["name"] as? String ?? "") }
        for (id, r) in asleep {
            let name = r["name"] as? String ?? id
            let item = NSMenuItem(title: name, action: nil, keyEquivalent: "")
            item.attributedTitle = NSAttributedString(string: name + " · off",
                                                      attributes: [.foregroundColor: NSColor.secondaryLabelColor])
            item.image = NSImage(systemSymbolName: "tv", accessibilityDescription: nil)
            item.submenu = asleepMenu(id: id, name: name, r)
            menu.addItem(item)
        }
        direct.addNearby(to: menu, online: online)
        menu.addItem(.separator())
        menu.addItem(findItem())
        menu.addItem(.run("Add Receiver by Link or Address…", symbol: "plus") { [weak self] in self?.askForLink() })
    }

    private func asleepMenu(id: String, name: String, _ r: [String: Any]) -> NSMenu {
        let m = NSMenu()
        let ways = KnownReceivers.methods(r)
        m.addItem(.run("Turn On", enabled: !ways.isEmpty, symbol: "power") { [weak self] in
            self?.background(name) { "Turning on: " + (try $0.finder.wake(id)).joined(separator: ", ") }
        })
        m.addItem(.label(ways.isEmpty ? KnownReceivers.hint(r) ?? "No way to wake it known yet" : "By " + ways.map {
            $0 == "wol" ? "Wake-on-LAN" : "the remote's Bluetooth signal, sent by another Luch device"
        }.joined(separator: " and ")))
        m.addItem(.separator())
        m.addItem(.run("Learn the Remote's Power Key…", symbol: "appletvremote.gen1") { [weak self] in
            let ok = Finding.alert(name, "Turn \(name) off, click Listen, then press the power key on its remote near this Mac (30 s).",
                                   buttons: ["Listen", "Cancel"])
            guard ok else { return }
            self?.background(name) { "Learned \(try $0.finder.learn(id, seconds: 30)): Wake turns it on now" }
        })
        return m
    }

    private func findItem() -> NSMenuItem {
        let parent = NSMenuItem(title: "Find Receivers", action: nil, keyEquivalent: "")
        parent.image = NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: nil)
        let m = NSMenu()
        let status = finder.status()
        if status["running"] as? Bool == true {
            for r in status["rungs"] as? [[String: Any]] ?? [] where r["state"] as? String == "running" {
                m.addItem(.label("… " + (r["note"] as? String ?? "")))
            }
            m.addItem(.separator())
        }
        let ways: [(String, String, String)] = [
            ("Search Again", "auto", "arrow.clockwise"),
            ("Search Harder: Also Sound and Camera", "all", "sparkle.magnifyingglass"),
            ("Ask Every Address of This Network", "scan", "network"),
            ("Listen over Bluetooth", "ble", "dot.radiowaves.right"),
            ("Hear the TV Say Its Address (Microphone)", "sound", "mic"),
            ("Read the TV's QR Code (Camera)", "qr", "qrcode.viewfinder"),
        ]
        for (title, mode, symbol) in ways {
            if mode == "scan" { m.addItem(.separator()) }
            m.addItem(.run(title, symbol: symbol) { [weak self] in self?.search(mode) })
        }
        direct.addFindItems(to: m)
        parent.submenu = m
        return parent
    }

    /// For an empty list: the whole ladder at once, the person's part (the TV's sound, its QR code) told first.
    func searchHarderItem() -> NSMenuItem {
        .run("Search Harder: Also Sound and Camera…", symbol: "sparkle.magnifyingglass") { [weak self] in self?.search("all") }
    }

    private func search(_ mode: String) {
        if mode == "sound" || mode == "all" {
            _ = Finding.alert("Find Receivers", "On the TV, open Luch and choose \"Say the address by sound\"; or hold this Mac's camera up to the QR code on its screen.", buttons: ["OK"])
        }
        do { try finder.start(mode) } catch { _ = Finding.alert("Find Receivers", error.localizedDescription, buttons: ["OK"]) }
        onChange?()
    }

    private func askForLink() {
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 300, height: 24))
        field.placeholderString = "luch://connect?… or 192.168.1.20"
        if let clip = NSPasteboard.general.string(forType: .string)?.trimmingCharacters(in: .whitespacesAndNewlines),
           clip.hasPrefix("luch://") || Card.parseLink(clip) != nil {
            field.stringValue = clip
        }
        let alert = NSAlert()
        alert.messageText = "Add Receiver"
        alert.informativeText = "The link of the TV's QR code, or its address (host or host:port)."
        alert.accessoryView = field
        alert.addButton(withTitle: "Add")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        alert.window.initialFirstResponder = field
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        let link = field.stringValue
        background("Add Receiver") { f in
            let id = try f.finder.add(link: link)
            return "Added \(f.finder.list().first { $0.id == id }?.name ?? id)"
        }
    }

    /// Runs `work` off the main thread and says how it went.
    private func background(_ title: String, _ work: @escaping (Finding) throws -> String) {
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let text: String
            do { text = try work(self) } catch { text = error.localizedDescription }
            DispatchQueue.main.async {
                _ = Finding.alert(title, text, buttons: ["OK"])
                self.onChange?()
            }
        }
    }

    /// True for the first button.
    @discardableResult
    static func alert(_ title: String, _ text: String, buttons: [String]) -> Bool {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = text
        buttons.forEach { alert.addButton(withTitle: $0) }
        NSApp.activate(ignoringOtherApps: true)
        return alert.runModal() == .alertFirstButtonReturn
    }
}
