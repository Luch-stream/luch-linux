// The app's side of connecting directly (LuchFind's Direct.swift): the local API's direct commands (luchctl direct),
// the question before this Mac leaves its Wi-Fi, "Connect Directly (Wi-Fi Direct)…" in Find Receivers, for each
// receiver heard over Bluetooth and in a receiver's menu, and a notice when it goes back to its network.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import LuchFind
import LuchNet
import UserNotifications

final class DirectUI {
    /// Answered on the local socket's thread: they wait for Bluetooth, the TV and the person.
    static let commands: Set<String> = ["direct_plan", "direct", "direct_status", "direct_off"]

    let direct: Direct
    private let finder: Finder
    var onChange: (() -> Void)? {
        didSet { direct.onChange = onChange }
    }

    init(finder: Finder, me: ProbeIdentity) {
        self.finder = finder
        direct = Direct(finder: finder, me: me)
        direct.confirm = { question in
            var yes = false
            DispatchQueue.main.sync { yes = Finding.alert("Connect Directly", question, buttons: ["Connect", "Cancel"]) }
            return yes
        }
        direct.onLeft = { words in DirectUI.notice(words) }
    }

    // MARK: - local API

    func handle(_ request: [String: Any]) -> [String: Any] {
        let key = (request["id"] as? String).flatMap { $0.isEmpty ? nil : $0 }
        do {
            switch request["cmd"] as? String ?? "" {
            case "direct_plan":
                let p = try direct.plan(key)
                return ["ok": true, "id": p.id, "name": p.name, "question": p.question]
            case "direct":
                // luchctl asked the question on its terminal; the attempt goes on here and `direct_status` follows it.
                Thread.detachNewThread { [direct] in _ = try? direct.connect(key, confirmed: request["confirmed"] as? Bool == true) }
                return ["ok": true]
            case "direct_status": return ["ok": true, "status": direct.status()]
            case "direct_off": return ["ok": true, "said": try direct.off()]
            default: return ["error": "unknown command"]
            }
        } catch {
            return ["error": error.localizedDescription]
        }
    }

    // MARK: - menu

    /// At the end of Find Receivers: connecting directly, and leaving the direct network while on it.
    func addFindItems(to m: NSMenu) {
        m.addItem(.separator())
        if let j = direct.wifi.joined {
            m.addItem(.label("Connected directly to \(j.name) on '\(j.ssid)'"))
            m.addItem(leaveItem(j))
        } else {
            m.addItem(.run("Connect Directly (Wi-Fi Direct)…", symbol: "wifi") { [weak self] in self?.connect(nil) })
        }
    }

    /// Receivers heard over Bluetooth that are not on this network, each with Connect Directly.
    func addNearby(to menu: NSMenu, online: [Receiver]) {
        for n in direct.status()["nearby"] as? [[String: Any]] ?? [] where n["listed"] as? Bool != true {
            let id = n["id"] as? String ?? ""
            let name = n["name"] as? String ?? id
            let item = NSMenuItem(title: name, action: nil, keyEquivalent: "")
            item.attributedTitle = NSAttributedString(string: name + " · Bluetooth only",
                                                      attributes: [.foregroundColor: NSColor.secondaryLabelColor])
            item.image = NSImage(systemSymbolName: "tv", accessibilityDescription: nil)
            let sub = NSMenu()
            sub.addItem(.label("Heard nearby, not on this network"))
            sub.addItem(.run("Connect Directly (Wi-Fi Direct)…", symbol: "wifi") { [weak self] in self?.connect(id) })
            item.submenu = sub
            menu.addItem(item)
        }
    }

    /// For a receiver's menu: Connect Directly when it was heard offering it, Leave while on its network.
    func items(for r: Receiver) -> [NSMenuItem] {
        if let j = direct.wifi.joined, j.receiverID == r.id { return [.separator(), leaveItem(j)] }
        guard let h = finder.directNearby().first(where: { r.id.replacingOccurrences(of: "-", with: "").lowercased().hasPrefix($0.card.id) })
        else { return [] }
        return [.separator(), .run("Connect Directly (Wi-Fi Direct)…", symbol: "wifi") { [weak self] in self?.connect(h.card.id) }]
    }

    private func leaveItem(_ j: DirectWiFi.Joined) -> NSMenuItem {
        .run("Leave '\(j.ssid)'" + (j.previous.map { " (Back to '\($0)')" } ?? ""), symbol: "wifi.slash") { [weak self] in
            self?.background { try $0.direct.off() }
        }
    }

    /// Asks (Direct's confirm), connects, and says how it went.
    private func connect(_ key: String?) {
        background { try $0.direct.connect(key, confirmed: false) }
    }

    private func background(_ work: @escaping (DirectUI) throws -> String) {
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let text: String
            do { text = try work(self) } catch { text = error.localizedDescription }
            DispatchQueue.main.async {
                _ = Finding.alert("Connect Directly", text, buttons: ["OK"])
                self.onChange?()
            }
        }
    }

    /// Going back by itself is said in a notification: the person may be away from the menu.
    private static func notice(_ words: String) {
        guard Bundle.main.bundleIdentifier != nil else { return }
        let center = UNUserNotificationCenter.current()
        center.requestAuthorization(options: [.alert]) { granted, _ in
            guard granted else { return }
            let content = UNMutableNotificationContent()
            content.title = "Back on Wi-Fi"
            content.body = words.prefix(1).uppercased() + words.dropFirst()
            center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
        }
    }
}
