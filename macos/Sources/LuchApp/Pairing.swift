// Pairing and receivers' reports in the app (PROTOCOL.md 3a, 6): the local API's commands for luchctl, the small
// window the TV's code is typed in, each TV's Paired / Forget in its menu, and a notification for a new report.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import LuchNet
import LuchProto
import UserNotifications

/// luchctl pair, paired, forget, reports and report, as luchd's D-Bus methods of those names.
enum PairingCommands {
    static let commands: Set<String> = ["pairing", "pair", "paired", "forget", "reports", "report_url", "dismiss_report"]

    static func handle(_ request: [String: Any], receivers: [Receiver]) -> [String: Any] {
        let id = request["id"] as? String ?? ""
        switch request["cmd"] as? String ?? "" {
        case "pairing":
            return ["ok": true, "state": Connector.shared.pairing(id)]
        case "pair":
            do {
                return ["ok": true, "name": try Connector.shared.pair(id, code: request["code"] as? String ?? "")]
            } catch {
                return ["error": error.localizedDescription]
            }
        case "paired":
            return ["ok": true, "paired": Connector.shared.paired()]
        case "forget":
            // A receiver that is off is still forgotten: by its name among the paired, or by its id.
            let key = request["name"] as? String ?? id
            let target = receivers.first { $0.id == key || $0.name == key }?.id
                ?? Connector.shared.paired().first { $0["name"] as? String == key }?["id"] as? String
                ?? key
            do {
                try Connector.shared.forget(target)
                return ["ok": true]
            } catch {
                return ["error": error.localizedDescription]
            }
        case "reports":
            return ["ok": true, "reports": Reports.list()]
        case "report_url":
            return ["ok": true, "url": Reports.list().first { $0["id"] as? String == id }.map(Reports.issueURL) ?? ""]
        case "dismiss_report":
            return Reports.remove(id) ? ["ok": true] : ["error": "no report \(id)"]
        default:
            return ["error": "unknown command"]
        }
    }
}

/// The code a TV shows, typed here: a small window that follows the pairing (allow on the TV, then the code), and
/// the action that found the TV asking, done again once paired (as GNOME does with luchd's PairChanged).
final class PairPrompt: NSObject {
    private var panel: NSPanel?
    /// The receiver the window is about, and its name.
    private var pairing: String?
    private var pairingName = ""
    private let title = NSTextField(labelWithString: "")
    private let message = NSTextField(wrappingLabelWithString: "")
    private let field = NSTextField(string: "")
    private let pairButton = NSButton(title: "Pair", target: nil, action: nil)
    /// id -> what to do again once paired.
    private var afterPairing: [String: () -> Void] = [:]
    /// ids whose code was just typed here: their outcome is told by the window itself.
    private var typing: Set<String> = []
    /// The menu follows what is paired.
    var onChange: (() -> Void)?

    func start() {
        Connector.shared.onPairChanged = { [weak self] id, state in
            DispatchQueue.main.async { self?.changed(id, state) }
        }
    }

    /// `action` found the receiver asking for pairing: it runs again once the code is in.
    func after(_ id: String, _ action: @escaping () -> Void) {
        afterPairing[id] = action
    }

    private func changed(_ id: String, _ state: [String: Any]) {
        let name = state["name"] as? String ?? id
        switch state["step"] as? String {
        case "allow":
            show(id, name: name, text: "\(name): allow the connection on the TV, then type the code it shows here.",
                 code: false)
        case "code":
            show(id, name: name, text: "Type the code the TV shows", code: true)
        case "paired":
            if !typing.contains(id) { finish(id, name: name) }
        case "failed":
            if !typing.contains(id) {
                afterPairing[id] = nil
                show(id, name: name, text: state["reason"] as? String ?? "That did not work", code: false)
            }
        default:
            break
        }
        onChange?()
    }

    private func finish(_ id: String, name: String) {
        if pairing == id { close() }
        log("app: paired with \(name)")
        afterPairing.removeValue(forKey: id)?()
    }

    private func show(_ id: String, name: String, text: String, code: Bool) {
        let panel = self.panel ?? makePanel()
        pairing = id
        pairingName = name
        title.stringValue = "Pair with \(name)"
        message.stringValue = text
        field.isHidden = !code
        field.stringValue = ""
        pairButton.isEnabled = code
        panel.title = "Luch"
        NSApp.activate(ignoringOtherApps: true)
        panel.makeKeyAndOrderFront(nil)
        if code { panel.makeFirstResponder(field) }
    }

    private func makePanel() -> NSPanel {
        let p = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 360, height: 176),
                        styleMask: [.titled, .closable], backing: .buffered, defer: false)
        p.isReleasedWhenClosed = false
        p.level = .floating
        let v = p.contentView!
        title.font = .boldSystemFont(ofSize: 14)
        title.frame = NSRect(x: 20, y: 140, width: 320, height: 20)
        message.frame = NSRect(x: 20, y: 96, width: 320, height: 38)
        field.placeholderString = "123 456"
        field.font = .monospacedDigitSystemFont(ofSize: 22, weight: .regular)
        field.alignment = .center
        field.frame = NSRect(x: 20, y: 54, width: 320, height: 34)
        field.target = self
        field.action = #selector(typed)
        pairButton.frame = NSRect(x: 250, y: 12, width: 90, height: 30)
        pairButton.bezelStyle = .rounded
        pairButton.keyEquivalent = "\r"
        pairButton.target = self
        pairButton.action = #selector(typed)
        let cancel = NSButton(title: "Cancel", target: self, action: #selector(cancelled))
        cancel.frame = NSRect(x: 156, y: 12, width: 90, height: 30)
        cancel.bezelStyle = .rounded
        cancel.keyEquivalent = "\u{1b}"
        [title, message, field, pairButton, cancel].forEach(v.addSubview)
        p.center()
        panel = p
        return p
    }

    @objc private func typed() {
        guard let id = pairing, !field.isHidden, !typing.contains(id) else { return }
        let code = field.stringValue
        let name = pairingName
        typing.insert(id)
        pairButton.isEnabled = false
        message.stringValue = "Checking the code…"
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let failure: String?
            do {
                try Connector.shared.pair(id, code: code)
                failure = nil
            } catch {
                failure = error.localizedDescription
            }
            DispatchQueue.main.async {
                guard let self else { return }
                self.typing.remove(id)
                if let failure, Connector.shared.pairing(id)["step"] as? String == "code" {
                    // Not six digits: the TV still waits for its code.
                    self.message.stringValue = failure
                    self.pairButton.isEnabled = true
                } else if let failure {
                    // A wrong code ends this pairing: the action again asks the TV for a new one.
                    self.field.isHidden = true
                    self.message.stringValue = failure
                    if let again = self.afterPairing.removeValue(forKey: id) {
                        self.pairButton.title = "Try Again"
                        self.pairButton.isEnabled = true
                        self.pairButton.action = #selector(self.tryAgain)
                        self.retry = again
                    }
                } else {
                    self.finish(id, name: name)
                }
                self.onChange?()
            }
        }
    }

    private var retry: (() -> Void)?

    @objc private func tryAgain() {
        pairButton.title = "Pair"
        pairButton.action = #selector(typed)
        let again = retry
        retry = nil
        if let id = pairing, let again { afterPairing[id] = again }
        again?()
    }

    @objc private func cancelled() {
        if let id = pairing { afterPairing[id] = nil }
        close()
    }

    private func close() {
        panel?.orderOut(nil)
        pairing = nil
        retry = nil
        pairButton.title = "Pair"
        pairButton.action = #selector(typed)
    }

    /// The per-TV menu's pairing lines: paired (and Forget), Pair…, or an older receiver without encryption.
    func menuItems(for r: Receiver, connect: @escaping () -> Void) -> [NSMenuItem] {
        if Connector.shared.isPaired(r.id) {
            return [.label("Paired · encrypted"),
                    .run("Forget Pairing", symbol: "lock.open") { [weak self] in
                        try? Connector.shared.forget(r.id)
                        self?.onChange?()
                    }]
        }
        if r.tlsPort == 0 { return [.label("Older Luch: not encrypted")] }
        return [.run("Pair…", symbol: "lock") { [weak self] in
            // Connecting shows the code on the TV; once paired the connection is made again, now trusted.
            self?.after(r.id, connect)
            connect()
        }]
    }
}

/// A notification for a receiver's new report; clicking it opens the issue to file, filled in.
final class ReportNotices: NSObject, UNUserNotificationCenterDelegate {
    static let shared = ReportNotices()

    func start() {
        // Outside an app bundle (swift run) the notification center is not there at all.
        guard Bundle.main.bundleIdentifier != nil else {
            Reports.onNew = { r, name in log("app: \(name) ran into a problem: \(Reports.summary(r))") }
            return
        }
        UNUserNotificationCenter.current().delegate = self
        Reports.onNew = { r, name in
            let center = UNUserNotificationCenter.current()
            center.requestAuthorization(options: [.alert]) { granted, _ in
                guard granted else { return log("app: \(name) ran into a problem: \(Reports.summary(r))") }
                let content = UNMutableNotificationContent()
                content.title = "\(name) ran into a problem"
                content.body = "\(Reports.summary(r))\nClick to file it as an issue; `luchctl reports` lists them all."
                content.userInfo = ["url": Reports.issueURL(r)]
                center.add(UNNotificationRequest(identifier: r["id"] as? String ?? UUID().uuidString,
                                                 content: content, trigger: nil))
            }
        }
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                withCompletionHandler done: @escaping () -> Void) {
        if let link = response.notification.request.content.userInfo["url"] as? String, let url = URL(string: link) {
            NSWorkspace.shared.open(url)
        }
        done()
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                withCompletionHandler done: @escaping (UNNotificationPresentationOptions) -> Void) {
        done([.banner])
    }
}
