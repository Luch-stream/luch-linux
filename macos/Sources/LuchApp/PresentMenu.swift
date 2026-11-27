// The menu's side of presenting: Present… per TV, its presenter view, the black screen, and the do-not-disturb
// setting; what luchctl present, blank and quiet do, for the mouse.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import LuchEngine
import LuchNet
import LuchPresent
import UniformTypeIdentifiers

extension AppDelegate {
    /// `engine(id)`: the mirroring session to that receiver, if this Mac mirrors to it (asked on the main thread).
    func setUpPresenting(engine: @escaping (String) -> Engine?) {
        caster.presenter.onChange = { [weak self] in self?.caster.onChange?() }
        // Banners back on if a crash left them off, and back on when the app quits mid-session.
        DispatchQueue.global(qos: .utility).async { _ = Quiet.shared }
        NotificationCenter.default.addObserver(forName: NSApplication.willTerminateNotification, object: nil,
                                               queue: nil) { _ in Quiet.shared.endAll() }
        caster.presenter.mirrorSend = { id, m in
            let e = Thread.isMainThread ? engine(id) : DispatchQueue.main.sync { engine(id) }
            guard let e, e.streaming, !e.options.soundOnly else { return false }
            _ = e.send(m)
            return true
        }
    }

    func presentingItems(_ r: Receiver, mirroringHere: Bool) -> [NSMenuItem] {
        var items: [NSMenuItem] = [.run("Present…", symbol: "play.rectangle") { [weak self] in self?.choosePresentation(for: r) }]
        if let url = caster.presenter.viewURL(r.id).flatMap(URL.init(string:)) {
            items.append(.run("Open Presenter View", symbol: "rectangle.inset.filled.and.person.filled") {
                NSWorkspace.shared.open(url)
            })
        }
        let blank = caster.presenter.blank(r.id)
        let shown = caster.existing(r.id)?.showing != nil || mirroringHere
        let black = NSMenuItem.run("Black Screen", enabled: shown, symbol: "rectangle.fill") { [weak self] in
            self?.send(["cmd": "annotate", "msg": ["t": "blank", "on": !blank]], to: r)
        }
        black.state = blank ? .on : .off
        items.append(black)
        return items
    }

    private func choosePresentation(for r: Receiver) {
        let panel = NSOpenPanel()
        panel.title = "Present on \(r.name)"
        panel.prompt = "Present"
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.allowedContentTypes = ["pdf", "pptx", "ppt", "odp"].compactMap { UTType(filenameExtension: $0) }
        NSApp.activate(ignoringOtherApps: true)
        guard panel.runModal() == .OK, let file = panel.url else { return }
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let response = self.caster.run(["cmd": "present", "file": file.path], on: r)
            DispatchQueue.main.async {
                if let url = (response["url"] as? String).flatMap(URL.init(string:)) {
                    NSWorkspace.shared.open(url)
                } else {
                    let alert = NSAlert()
                    alert.messageText = r.name
                    alert.informativeText = response["error"] as? String ?? "could not present \(file.lastPathComponent)"
                    NSApp.activate(ignoringOtherApps: true)
                    alert.runModal()
                }
                self.caster.onChange?()
            }
        }
    }

    /// Do not disturb while presenting or mirroring: ask once, always, or never.
    func quietItem() -> NSMenuItem {
        let parent = NSMenuItem(title: "Do Not Disturb While Presenting", action: nil, keyEquivalent: "")
        let menu = NSMenu()
        let now = Quiet.shared.mode
        for (mode, title) in [(QuietMode.ask, "Ask Once"), (.on, "On"), (.off, "Off")] {
            let item = NSMenuItem.run(title) { [weak self] in
                DispatchQueue.global(qos: .utility).async {
                    _ = self?.caster.presenter.quiet(mode.rawValue)
                    self?.caster.onChange?()
                }
            }
            item.state = mode == now ? .on : .off
            menu.addItem(item)
        }
        parent.submenu = menu
        return parent
    }
}
