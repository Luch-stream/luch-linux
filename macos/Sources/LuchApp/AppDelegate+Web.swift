// The web receiver in the app: started with it, the question about a new browser shown here with its code, its
// browsers in the receiver list (mirrored to at once unless their page asks not to), and its address in the menu.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import LuchNet
import LuchProto
import LuchWeb

/// Allowed browsers and the HTTPS identity live next to the control socket.
let webReceiver = WebReceiver(dir: FileManager.default.homeDirectoryForCurrentUser
    .appendingPathComponent("Library/Application Support/Luch", isDirectory: true))
/// The question on screen now, to take away when it is answered elsewhere or nobody answered in time.
private var askingAbout: String?

extension AppDelegate {
    func startWebReceiver() {
        webReceiver.onAsk = { [weak self] q in DispatchQueue.main.async { self?.askAboutBrowser(q) } }
        webReceiver.onAskDone = { q in
            DispatchQueue.main.async { if askingAbout == q.id { NSApp.abortModal() } }
        }
        webReceiver.onReceiver = { [weak self] r, mode in
            DispatchQueue.main.async {
                guard let self else { return }
                self.refreshReceivers()
                // One tap on the TV starts the whole thing; `?mode=none` keeps it manual.
                if mode != "none" { self.startMirror(r, extend: mode == "virtual") }
            }
        }
        webReceiver.onGone = { [weak self] _ in DispatchQueue.main.async { self?.refreshReceivers() } }
        Thread.detachNewThread {
            do { try webReceiver.start() } catch { log("web: receiver off: \(error.localizedDescription)") }
        }
    }

    /// Numeric comparison: the person lets in the browser that shows the same code (the TV in front of them).
    private func askAboutBrowser(_ q: WebReceiver.Question) {
        guard webReceiver.isWaiting(q.id) else { return }
        let alert = NSAlert()
        alert.messageText = "Let a browser watch this screen? Code \(q.code)"
        alert.informativeText = "A browser at \(q.address) (\(q.name)) wants to show this screen. Allow it only if it shows the code \(q.code)."
        alert.addButton(withTitle: "Allow")
        alert.addButton(withTitle: "Deny")
        askingAbout = q.id
        NSApp.activate(ignoringOtherApps: true)
        let answer = alert.runModal()
        askingAbout = nil
        if answer == .alertFirstButtonReturn { webReceiver.answer(q.id, allow: true) }
        if answer == .alertSecondButtonReturn { webReceiver.answer(q.id, allow: false) }
    }

    /// The page's address, to open in a TV's browser.
    func webMenuItem() -> NSMenuItem {
        let parent = NSMenuItem(title: "Web Receiver", action: nil, keyEquivalent: "")
        parent.image = NSImage(systemSymbolName: "globe", accessibilityDescription: nil)
        let m = NSMenu()
        if let url = webReceiver.url {
            m.addItem(.label("Open in a browser on the TV:"))
            m.addItem(.label(url))
            m.addItem(.run("Copy Address", symbol: "doc.on.doc") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(url, forType: .string)
            })
        } else {
            m.addItem(.label("No network"))
        }
        let allowed = (webReceiver.handle(["cmd": "web_clients"])["clients"] as? [[String: Any]] ?? [])
            .filter { $0["waiting"] as? Bool != true }
        if !allowed.isEmpty {
            m.addItem(.separator())
            m.addItem(.label("Allowed browsers"))
            for b in allowed {
                let id = b["id"] as? String ?? ""
                m.addItem(.run("Forget \(b["name"] as? String ?? "?") (\(b["address"] as? String ?? ""))", symbol: "xmark") {
                    _ = webReceiver.handle(["cmd": "forget_web", "id": id])
                })
            }
        }
        parent.submenu = m
        return parent
    }

    /// A browser's menu: it only shows this screen.
    func webBrowserMenu(_ r: Receiver) -> NSMenu {
        let m = NSMenu()
        m.addItem(.label("A browser · this screen only"))
        m.addItem(.run("Mirror Screen", symbol: "rectangle.on.rectangle") { [weak self] in self?.startMirror(r) })
        m.addItem(.run("Stop Mirroring", symbol: "stop.circle") { [weak self] in self?.stopMirror() })
        return m
    }
}
