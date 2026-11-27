// What a receiver shows of this Mac besides the whole screen (one window, one of several displays), and whether the
// TV may drive this Mac (PROTOCOL.md 4i): the menu's items, luchctl's `mirror [r] [source] [which]`, and the question.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import CoreGraphics
import LuchEngine
import LuchLocal
import LuchNet
import LuchProto

/// Whether a TV may drive this Mac: ask the first time in a session (the default), always, or never.
enum InputChoice: String {
    case ask, always, off
}

extension Preferences {
    func inputChoice(for id: String) -> InputChoice {
        UserDefaults.standard.string(forKey: "input." + id).flatMap(InputChoice.init(rawValue:)) ?? .ask
    }

    func setInputChoice(_ c: InputChoice, for id: String) {
        UserDefaults.standard.set(c.rawValue, forKey: "input." + id)
    }

    /// The display a TV gets "as it is", by name (ids change when a monitor is plugged in again); nil: the main one.
    func displayName(for id: String) -> String? {
        UserDefaults.standard.string(forKey: "display." + id)
    }

    func setDisplayName(_ name: String?, for id: String) {
        UserDefaults.standard.set(name, forKey: "display." + id)
    }
}

/// This Mac's displays, as the person knows them.
enum Displays {
    struct Entry {
        var id: CGDirectDisplayID
        var name: String
    }

    static func all() -> [Entry] {
        NSScreen.screens.compactMap { s in
            (s.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)
                .map { Entry(id: $0.uint32Value, name: s.localizedName) }
        }
    }

    static func id(named name: String?) -> CGDirectDisplayID? {
        name.flatMap { n in all().first { $0.name == n }?.id }
    }
}

/// The windows one could show alone, from the window server (no wait, unlike ScreenCaptureKit's list).
enum Windows {
    struct Entry {
        var id: CGWindowID
        var name: String
    }

    static func all() -> [Entry] {
        let me = ProcessInfo.processInfo.processIdentifier
        let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
            as? [[String: Any]] ?? []
        return info.compactMap { w in
            guard (w[kCGWindowLayer as String] as? Int) == 0, (w[kCGWindowOwnerPID as String] as? Int32) != me,
                  let id = w[kCGWindowNumber as String] as? CGWindowID,
                  let title = w[kCGWindowName as String] as? String, !title.isEmpty,
                  let b = (w[kCGWindowBounds as String] as? NSDictionary).flatMap({ CGRect(dictionaryRepresentation: $0) }),
                  b.width >= 100, b.height >= 100 else { return nil }
            let app = w[kCGWindowOwnerName as String] as? String ?? "?"
            return Entry(id: id, name: "\(app) — \(title)")
        }
    }
}

extension AppDelegate {
    // MARK: - the TV's input

    /// The input of a session with `r`, or nil when the person said never (the receiver then never asks).
    func inputControl(for r: Receiver) -> InputControl? {
        let choice = Preferences.shared.inputChoice(for: r.id)
        guard choice != .off else { return nil }
        return InputControl(always: choice == .always, ask: Self.askInput, remember: { a in
            if a == .always { DispatchQueue.main.async { Preferences.shared.setInputChoice(.always, for: r.id) } }
        })
    }

    /// "May Living room TV control this Mac?": allow once, always, or not. No answer in a minute is a no.
    static func askInput(_ receiver: String, _ devices: [String], _ reply: @escaping (InputAnswer) -> Void) {
        DispatchQueue.main.async {
            let alert = NSAlert()
            alert.messageText = "May \(receiver) control this Mac?"
            let what = devices.isEmpty ? "a keyboard, mouse or touch screen" : devices.joined(separator: ", ")
            alert.informativeText = "It wants to use its \(what) on this Mac: the pointer and the keys act where "
                + "the TV shows. You can take it back from the Luch menu."
                + (InputPoster.allowed ? "" : "\n\nmacOS then asks once to let Luch control this Mac "
                    + "(Privacy & Security › Accessibility).")
            alert.addButton(withTitle: "Allow Once")
            alert.addButton(withTitle: "Always Allow")
            alert.addButton(withTitle: "Don't Allow")
            let timeout = Timer(timeInterval: 60, repeats: false) { _ in NSApp.abortModal() }
            RunLoop.main.add(timeout, forMode: .modalPanel)
            NSApp.activate(ignoringOtherApps: true)
            let answer = alert.runModal()
            timeout.invalidate()
            switch answer {
            case .alertFirstButtonReturn: reply(.once)
            case .alertSecondButtonReturn: reply(.always)
            default: reply(.no)
            }
        }
    }

    /// The per-TV switch, "without asking" once it is on, and taking control back during a session.
    func inputItems(for r: Receiver) -> [NSMenuItem] {
        let choice = Preferences.shared.inputChoice(for: r.id)
        let control = currentReceiverID == r.id ? engine?.input : nil
        let allow = NSMenuItem.run("Allow TV to Control This Mac", symbol: "keyboard") {
            Preferences.shared.setInputChoice(choice == .off ? .ask : .off, for: r.id)
            if choice != .off { control?.takeBack() }
        }
        allow.toolTip = "A keyboard, mouse or touch screen on the TV drives this Mac while it shows it; asked first"
        allow.state = choice == .off ? .off : .on
        var items = [allow]
        if choice != .off {
            let always = NSMenuItem.run("Without Asking") {
                Preferences.shared.setInputChoice(choice == .always ? .ask : .always, for: r.id)
            }
            always.indentationLevel = 1
            always.state = choice == .always ? .on : .off
            items.append(always)
        }
        if control?.granted == true {
            let back = NSMenuItem.run("Take Back Control", symbol: "hand.raised") { control?.takeBack() }
            back.indentationLevel = 1
            items.append(back)
        }
        return items
    }

    // MARK: - one window, one display

    /// The Screen submenu's sources after its modes: a window of this Mac, and the display when there are several.
    func addScreenSources(to menu: NSMenu, for r: Receiver) {
        menu.addItem(.separator())
        let windows = NSMenuItem(title: "One Window", action: nil, keyEquivalent: "")
        windows.image = NSImage(systemSymbolName: "macwindow", accessibilityDescription: nil)
        let list = NSMenu()
        let all = Windows.all()
        for w in all {
            list.addItem(.run(w.name) { [weak self] in self?.show(window: w.id, on: r) })
        }
        if all.isEmpty { list.addItem(.label("No windows (allow Screen Recording first)")) }
        windows.submenu = list
        menu.addItem(windows)

        let displays = Displays.all()
        guard displays.count > 1 else { return }
        menu.addItem(.separator())
        let chosen = Preferences.shared.displayName(for: r.id)
        let mode = Preferences.shared.screenMode(for: r.id)
        for (i, d) in displays.enumerated() {
            let item = NSMenuItem.run("As It Is: \(d.name)") { [weak self] in
                Preferences.shared.setDisplayName(i == 0 ? nil : d.name, for: r.id)
                Preferences.shared.setScreenMode(.asIs, for: r.id)
                self?.switchScreen(.asIs, on: r)
            }
            item.indentationLevel = 1
            if mode == .asIs, (chosen ?? displays[0].name) == d.name { item.state = .on }
            menu.addItem(item)
        }
    }

    /// The display "as it is" shows for `r`.
    func displayID(for r: Receiver) -> CGDirectDisplayID? {
        Displays.id(named: Preferences.shared.displayName(for: r.id))
    }

    private func show(window: CGWindowID, on r: Receiver) {
        if currentReceiverID == r.id, engine?.streaming == true {
            switchScreen(.asIs, on: r, window: window)
        } else {
            startMirror(r, extend: false, window: window)
        }
    }

    // MARK: - luchctl

    /// `mirror [r] <source> [which]`: luchd's words and the Mac's.
    func mirror(_ source: MirrorSource, on r: Receiver) -> [String: Any] {
        let ok: [String: Any] = ["ok": true, "receiver": r.name]
        switch source {
        case .asIs: startMirror(r, extend: false)
        case .extend: startMirror(r, extend: true)
        case let .ultra(word):
            guard let u = UltraPriority(rawValue: word) else { return ["error": "ultra: speed, quality or mac"] }
            startMirror(r, extend: false, ultra: u)
        case .audio: startMirror(r, soundOnly: true)
        case let .monitor(which):
            let all = Displays.all()
            return pick(which, all.map { ReceiverEntry(id: String($0.id), name: $0.name) }, what: "display", ok: ok) {
                [weak self] id in self?.startMirror(r, extend: false, displayID: CGDirectDisplayID(id) ?? 0)
            }
        case let .window(which):
            return pick(which, Windows.all().map { ReceiverEntry(id: String($0.id), name: $0.name) }, what: "window",
                        ok: ok) { [weak self] id in self?.startMirror(r, extend: false, window: CGWindowID(id) ?? 0) }
        }
        return ok
    }

    /// `displays` and `windows` for luchctl to ask on its terminal.
    func sources(_ what: String) -> [String: Any] {
        let all = what == "windows" ? Windows.all().map { (String($0.id), $0.name) }
            : Displays.all().map { (String($0.id), $0.name) }
        return [what: all.map { ["id": $0.0, "name": $0.1] }]
    }

    /// Named: that one; one only: it; else the person picks on this Mac.
    private func pick(_ which: String?, _ all: [ReceiverEntry], what: String, ok: [String: Any],
                      then: @escaping (String) -> Void) -> [String: Any] {
        if let which {
            // "#<id>": what luchctl picked from the list on its terminal.
            let byID = which.hasPrefix("#") ? all.first { "#" + $0.id == which } : nil
            guard let e = byID ?? SourceChoice.pick(which, from: all) else {
                return ["error": "no such \(what): \(which) (\(all.map(\.name).joined(separator: ", ")))"]
            }
            then(e.id)
            return ok
        }
        guard all.count > 1 else {
            guard let e = all.first else { return ["error": "no \(what) to show"] }
            then(e.id)
            return ok
        }
        DispatchQueue.main.async {
            if let i = Self.choose("Which \(what) should the TV show?", all.map(\.name)) { then(all[i].id) }
        }
        var asking = ok
        asking["asking"] = "choose the \(what) on the Mac"
        return asking
    }

    /// A question with a list to pick from; nil when cancelled.
    private static func choose(_ question: String, _ names: [String]) -> Int? {
        let alert = NSAlert()
        alert.messageText = question
        let popup = NSPopUpButton(frame: NSRect(x: 0, y: 0, width: 320, height: 26))
        popup.addItems(withTitles: names)
        alert.accessoryView = popup
        alert.addButton(withTitle: "Show")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        return alert.runModal() == .alertFirstButtonReturn ? popup.indexOfSelectedItem : nil
    }
}
