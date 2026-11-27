// The menu of a TV without Luch (a Chromecast, a DLNA TV): what it can do, for the mouse, as luchctl does it.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import LuchNet
import LuchTV

extension AppDelegate {
    func tvMenu(_ r: Receiver) -> NSMenu {
        let kind = TVKind.of(r.id)
        let m = NSMenu()
        m.addItem(.label(kind == .cast ? "Google Cast · files and links" : "DLNA · files and links"))
        m.addItem(.run("Show File…", symbol: "doc") { [weak self] in self?.chooseFiles(for: r) })
        let link = Self.clipboardLink()
        let linkTitle = link.flatMap { URL(string: $0)?.host }.map { "Show Link from Clipboard (\($0))" }
            ?? "Show Link from Clipboard"
        m.addItem(.run(linkTitle, enabled: link != nil, symbol: "link") { [weak self] in
            if let link { self?.send(["cmd": "cast", "items": [link]], to: r) }
        })
        guard let s = caster.tvs.session(r.id) else { return m }

        m.addItem(.separator())
        let state = s.state
        let playing = state["state"] as? String == "playing"
        var line = (state["state"] as? String ?? "loading").capitalized
        if let pos = state["pos"] as? Double {
            line += " · " + Self.clock(pos)
            if let dur = state["dur"] as? Double { line += " / " + Self.clock(dur) }
        }
        m.addItem(.label(line))
        m.addItem(.run(playing ? "Pause" : "Play", symbol: playing ? "pause.fill" : "play.fill") { [weak self] in
            self?.send(["cmd": "media_cmd", "media": ["do": "toggle"]], to: r)
        })
        m.addItem(.run("Back 10 s", symbol: "gobackward.10") { [weak self] in
            self?.send(["cmd": "media_cmd", "media": ["do": "skip", "by": -10]], to: r)
        })
        m.addItem(.run("Forward 30 s", symbol: "goforward.30") { [weak self] in
            self?.send(["cmd": "media_cmd", "media": ["do": "skip", "by": 30]], to: r)
        })
        if s.device.kind.can.contains("rate") {
            let speed = NSMenuItem(title: "Speed", action: nil, keyEquivalent: "")
            let speeds = NSMenu()
            let now = state["rate"] as? Double ?? 1
            for rate in [0.75, 1, 1.25, 1.5, 2] {
                let item = NSMenuItem.run(rate == 1 ? "Normal" : String(format: "%g×", rate)) { [weak self] in
                    self?.send(["cmd": "media_cmd", "media": ["do": "rate", "rate": rate]], to: r)
                }
                if abs(rate - now) < 0.01 { item.state = .on }
                speeds.addItem(item)
            }
            speed.submenu = speeds
            m.addItem(speed)
        }
        m.addItem(.run("Stop Showing", symbol: "xmark.circle") { [weak self] in self?.send(["cmd": "stop_cast"], to: r) })

        // The volume of such a TV is reached through what plays on it.
        m.addItem(.separator())
        let parent = NSMenuItem(title: "Volume", action: nil, keyEquivalent: "")
        parent.image = NSImage(systemSymbolName: "speaker.wave.2", accessibilityDescription: nil)
        let level = (s.volume?["level"] as? NSNumber)?.intValue
        let muted = s.volume?["muted"] as? Bool ?? false
        if let level { parent.title = muted ? "Volume · muted" : "Volume · \(level)%" }
        let menu = NSMenu()
        let row = NSView(frame: NSRect(x: 0, y: 0, width: 220, height: 26))
        let slider = NSSlider(value: Double(level ?? 0), minValue: 0, maxValue: 100, target: nil, action: nil)
        slider.isContinuous = false
        slider.controlSize = .small
        slider.frame = NSRect(x: 20, y: 4, width: 186, height: 18)
        let act = MenuAction { [weak self, weak slider] in
            guard let slider else { return }
            self?.send(["cmd": "volume", "what": String(Int(slider.doubleValue.rounded()))], to: r)
        }
        slider.target = act
        slider.action = #selector(MenuAction.fire)
        row.addSubview(slider)
        let sliderItem = NSMenuItem()
        sliderItem.view = row
        sliderItem.representedObject = act
        menu.addItem(sliderItem)
        menu.addItem(.run("Louder", symbol: "speaker.plus") { [weak self] in self?.send(["cmd": "volume", "what": "+"], to: r) })
        menu.addItem(.run("Quieter", symbol: "speaker.minus") { [weak self] in self?.send(["cmd": "volume", "what": "-"], to: r) })
        menu.addItem(.run(muted ? "Unmute" : "Mute", symbol: muted ? "speaker.wave.2" : "speaker.slash") { [weak self] in
            self?.send(["cmd": "volume", "what": muted ? "unmute" : "mute"], to: r)
        })
        parent.submenu = menu
        m.addItem(parent)
        return m
    }
}
