// Playing in step on several receivers (PROTOCOL.md 4k), each receiver's sound mode and correction, and the latency
// lab (4j): the local API's `group`, `offset`, `audio` and `latency` commands, and their places in the menu.
// Runs the commands on the local socket's thread (a group waits for every receiver); AppKit only on the main one.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import LuchEngine
import LuchNet
import LuchProto

final class Together {
    static let commands: Set<String> = ["group", "group_stats", "offset", "audio", "latency", "calibrate"]

    /// What the app around it knows and does; set once at launch, called on the main thread.
    struct Hooks {
        /// The receivers on the LAN now.
        var receivers: () -> [Receiver] = { [] }
        /// The single session running, its receiver and how its screen is shown ("asis", "extend", "audio", ...).
        var mirroring: () -> (id: String, engine: Engine, mode: String)? = { nil }
        /// Ends the single session (one capture at a time: a group replaces it).
        var stopMirror: () -> Void = {}
        /// Mirror options as the menu has them set.
        var options: () -> MirrorOptions = { MirrorOptions() }
        /// Starts the single session to this receiver again (its sound mode changed in the menu).
        var restart: (String) -> Void = { _ in }
        /// Something here changed: the menu follows.
        var changed: () -> Void = {}
    }

    var hooks = Hooks()
    let store = SyncStore.shared
    /// A calibration runs (Calibrate.swift).
    let calibrating = OneAtATime()
    private let lock = NSLock()
    private var group: GroupSession?
    private var lab: LatencyLab?
    /// What the group is doing, for the menu: connecting, playing, or why it ended.
    private var status = ""
    private weak var statusItem: NSMenuItem?

    // MARK: - commands

    /// The local API's commands this answers, and "stop" while a group plays (as luchd's: stopping a member ends the
    /// group); nil for the others. On the socket's thread.
    func intercept(_ request: [String: Any]) -> [String: Any]? {
        let cmd = request["cmd"] as? String ?? ""
        if Self.commands.contains(cmd) { return handle(request) }
        // A group and a single session never play at once: while a group plays, its numbers are the stats.
        if cmd == "stats", let g = currentGroup() {
            let all = g.stats
            var flat = all["sender"] as? [String: Any] ?? [:]
            flat["est_ms"] = all["est_ms"]
            flat["decode_ms"] = (all["receiver"] as? [String: Any])?["decode_ms"]
            return ["stats": flat]
        }
        guard cmd == "stop", let g = currentGroup() else { return nil }
        if let key = (request["id"] as? String) ?? (request["name"] as? String),
           let r = try? find(key), !g.receiverIDs.contains(r.id) {
            return nil
        }
        stop()
        return ["ok": true]
    }

    private func handle(_ request: [String: Any]) -> [String: Any] {
        do {
            switch request["cmd"] as? String ?? "" {
            case "group": return try startGroup(request)
            case "group_stats": return ["group": currentGroup()?.stats ?? [:]]
            case "offset": return try setOffset(request)
            case "audio": return try setAudio(request)
            case "latency": return try latency(request)
            case "calibrate": return try calibrate()
            default: return ["error": "unknown command"]
            }
        } catch {
            return ["error": error.localizedDescription]
        }
    }

    func onMain<T>(_ run: () -> T) -> T {
        Thread.isMainThread ? run() : DispatchQueue.main.sync(execute: run)
    }

    func currentGroup() -> GroupSession? {
        lock.lock()
        defer { lock.unlock() }
        return group
    }

    /// A receiver by id or name (any case), or the only one on the LAN when none is named.
    private func find(_ key: String?) throws -> Receiver {
        let all = onMain(hooks.receivers)
        guard let key, !key.isEmpty else {
            switch all.count {
            case 1: return all[0]
            case 0: throw Failure("no receivers found (is the receiver app running?)")
            default: throw Failure("several receivers, name one: \(all.map(\.name).joined(separator: ", "))")
            }
        }
        let hits = all.filter { $0.id == key || $0.name.caseInsensitiveCompare(key) == .orderedSame }
        switch hits.count {
        case 1: return hits[0]
        case 0: throw Failure("no receiver '\(key)' (see `luchctl list`)")
        default: throw Failure("'\(key)' is ambiguous, use the id")
        }
    }

    private func startGroup(_ request: [String: Any]) throws -> [String: Any] {
        let modeText = request["mode"] as? String ?? ""
        guard let mode = SyncMode.parse(modeText) else { throw Failure("mode must be interactive|watch, not \(modeText)") }
        let source = request["source"] as? String ?? "fast"
        guard ["fast", "monitor", "audio"].contains(source) else {
            throw Failure("a group plays the screen as it is or the sound only: --source fast|monitor|audio, not \(source)")
        }
        let asked = request["members"] as? [[String: Any]] ?? []
        guard asked.count >= 2 else { throw Failure("two or more receivers, one role each") }
        var members: [GroupMember] = []
        for (i, m) in asked.enumerated() {
            let roleText = m["role"] as? String ?? (i == 0 ? "both" : "audio")
            guard let role = GroupRole(rawValue: roleText) else { throw Failure("role video|audio|both, not \(roleText)") }
            members.append(GroupMember(try find(m["r"] as? String), role: role))
        }
        try play(members, mode: mode, soundOnly: source == "audio")
        return ["ok": true]
    }

    /// Ends what plays (one capture at a time), then connects to every member; blocks until all play or one refused.
    private func play(_ members: [GroupMember], mode: SyncMode, soundOnly: Bool) throws {
        let options = onMain { () -> MirrorOptions in
            hooks.stopMirror()
            return hooks.options()
        }
        stop()
        let s = GroupSession(members: members, mode: mode, soundOnly: soundOnly, options: options, store: store)
        s.onWaitingForAllow = { [weak self] name in self?.show("Press Allow on \(name) (left, then OK on its remote)") }
        s.onStats = { [weak self] stats in self?.showDelay(stats) }
        s.onStopped = { [weak self, weak s] reason in
            guard let self else { return }
            self.lock.lock()
            if self.group === s { self.group = nil }
            self.lock.unlock()
            self.show(reason == "menu" ? "" : "Ended: \(reason)")
        }
        show("Connecting to \(members.map(\.receiver.name).joined(separator: ", "))…")
        do {
            try s.start()
        } catch {
            show("Error: \(error.localizedDescription)")
            throw error
        }
        lock.lock()
        group = s
        let labID = lab?.id
        lock.unlock()
        if let labID { s.send(["t": "latency", "on": true], to: labID) }
        show("Playing")
    }

    /// Ends the group, if one plays.
    func stop() {
        lock.lock()
        let g = group
        group = nil
        lock.unlock()
        g?.stop(reason: "menu")
    }

    private func setOffset(_ request: [String: Any]) throws -> [String: Any] {
        let ms = (request["ms"] as? NSNumber)?.doubleValue ?? .nan
        guard (-1000...1000).contains(ms) else { throw Failure("offset \(request["ms"] ?? "") ms is beyond ±1 s") }
        let r = try find(request["id"] as? String)
        offset(ms, for: r.id)
        return ["ok": true]
    }

    /// A receiver's own correction (positive = later) for the output route it uses now: saved, and applied at once
    /// when it plays.
    private func offset(_ ms: Double, for id: String) {
        store.setOffset(ms, key: Sync.offsetKey(id, route: store.route(id)))
        if currentGroup()?.setOffset(ms, for: id) == true { return }
        onMain {
            if let m = hooks.mirroring(), m.id == id { m.engine.send(["t": "offset", "ms": ms]) }
        }
    }

    private func setAudio(_ request: [String: Any]) throws -> [String: Any] {
        let text = request["mode"] as? String ?? ""
        guard let mode = AudioMode(rawValue: text) else { throw Failure("audio must be auto|on|off|sync, not \(text)") }
        store.setAudioMode(mode, for: try find(request["id"] as? String).id)
        return ["ok": true]
    }

    /// On: the page's address (to open full screen on this Mac), and the receiver draws its own pattern over the
    /// mirrored picture. Off: both go. No receiver named: the one that plays, when it is one.
    private func latency(_ request: [String: Any]) throws -> [String: Any] {
        let on = request["on"] as? Bool ?? true
        let id: String
        if let key = request["id"] as? String, !key.isEmpty {
            id = try find(key).id
        } else {
            var playing = currentGroup()?.receiverIDs ?? []
            if let m = onMain(hooks.mirroring) { playing.append(m.id) }
            lock.lock()
            let labID = lab?.id
            lock.unlock()
            guard let one = playing.count == 1 ? playing[0] : labID else {
                throw Failure("name the receiver: none or several are mirroring")
            }
            id = one
        }
        return ["url": try setLab(on, for: id)]
    }

    @discardableResult
    private func setLab(_ on: Bool, for id: String) throws -> String {
        let next = on ? try LatencyLab(id: id) { [weak self] in self?.labNow(id) ?? ([:], "") } : nil
        lock.lock()
        let old = lab
        lab = next
        lock.unlock()
        old?.close()
        if let old, old.id != id { tell(["t": "latency", "on": false], to: old.id) }
        tell(["t": "latency", "on": on], to: id)
        onMain(hooks.changed)
        return next?.url ?? ""
    }

    /// The flag holds for the connection it came on: the session's, so it is sent again when one starts.
    private func tell(_ message: [String: Any], to id: String) {
        if currentGroup()?.send(message, to: id) == true { return }
        onMain {
            if let m = hooks.mirroring(), m.id == id { m.engine.send(message) }
        }
    }

    /// A single session started (the app calls this): the lab's flag goes to its receiver.
    func sessionStarted(_ id: String) {
        lock.lock()
        let labID = lab?.id
        lock.unlock()
        if labID == id { tell(["t": "latency", "on": true], to: id) }
    }

    /// The numbers the lab's QR code tells, in luchd's shape.
    private func labNow(_ id: String) -> ([String: Any], String) {
        if let g = currentGroup(), g.receiverIDs.contains(id) {
            return (g.stats, "group-\(g.mode.wire)")
        }
        return onMain {
            guard let m = hooks.mirroring(), m.id == id, let s = m.engine.lastStats else { return ([:], "") }
            var sender: [String: Any] = ["rtt_ms": s.rttMs, "host_ms": s.hostMs, "mbps": s.mbps]
            if let plan = m.engine.plan {
                sender["encoder"] = plan.codec == .hevc ? "hevc_videotoolbox" : "h264_videotoolbox"
                sender["res"] = "\(plan.width)x\(plan.height)"
            }
            return (["sender": sender, "est_ms": s.estMs], m.mode)
        }
    }

    // MARK: - menu

    func show(_ text: String) {
        lock.lock()
        status = text
        lock.unlock()
        DispatchQueue.main.async { [weak self] in self?.hooks.changed() }
    }

    private func showDelay(_ stats: [String: Any]) {
        let line = Self.delayLine(stats["group"] as? [String: Any] ?? [:])
        DispatchQueue.main.async { [weak self] in self?.statusItem?.title = line }
    }

    /// "In step · D 150 ms · 3 receivers", the picture's D too when it differs.
    static func delayLine(_ g: [String: Any]) -> String {
        let dv = (g["d_video_ms"] as? NSNumber)?.doubleValue ?? 0
        let da = (g["d_audio_ms"] as? NSNumber)?.doubleValue ?? 0
        let n = (g["members"] as? [[String: Any]] ?? []).filter { $0["present"] as? Bool == true }.count
        let d = dv == da ? String(format: "D %.0f ms", da) : String(format: "D picture %.0f, sound %.0f ms", dv, da)
        return "In step · \(d) · \(n) receivers"
    }

    /// The top of the menu: Play Together, and while a group plays its D and Stop.
    func addMenuItems(to menu: NSMenu, receivers: [Receiver]) {
        lock.lock()
        let (g, text) = (group, status)
        lock.unlock()
        menu.addItem(.run("Play Together…", enabled: receivers.count >= 2, symbol: "hifispeaker.2") { [weak self] in
            self?.choose(from: receivers)
        })
        if let g {
            let names = receivers.filter { g.receiverIDs.contains($0.id) }.map(\.name).joined(separator: ", ")
            menu.addItem(.label("\(g.soundOnly ? "Sound" : "Screen") on \(names) (\(g.mode == .watching ? "watching" : "interactive"))"))
            let item = NSMenuItem.label(Self.delayLine(g.stats["group"] as? [String: Any] ?? [:]))
            statusItem = item
            menu.addItem(item)
            menu.addItem(calibrateItem(g))
            menu.addItem(.run("Stop Playing Together", symbol: "stop.circle") { [weak self] in
                DispatchQueue.global(qos: .userInitiated).async { self?.stop() }
            })
        } else if !text.isEmpty {
            menu.addItem(.label(text))
        }
    }

    /// A receiver's own items: its sound, its correction in a group, the latency lab.
    func addReceiverItems(to m: NSMenu, receiver r: Receiver) {
        let current = store.audioMode(r.id)
        let sound = NSMenuItem(title: "Sound: \(current.rawValue)", action: nil, keyEquivalent: "")
        let soundMenu = NSMenu()
        for mode in AudioMode.allCases {
            let item = NSMenuItem.run(mode.label) { [weak self] in
                guard let self else { return }
                self.store.setAudioMode(mode, for: r.id)
                if let m = self.hooks.mirroring(), m.id == r.id { self.hooks.restart(r.id) } else { self.hooks.changed() }
            }
            if mode == current { item.state = .on }
            soundMenu.addItem(item)
        }
        sound.submenu = soundMenu
        m.addItem(sound)
        m.addItem(offsetItem(r))
        lock.lock()
        let labOn = lab?.id == r.id
        lock.unlock()
        let labItem = NSMenuItem.run("Latency Lab", symbol: "stopwatch") { [weak self] in
            DispatchQueue.global(qos: .userInitiated).async {
                guard let url = try? self?.setLab(!labOn, for: r.id), let link = URL(string: url) else { return }
                DispatchQueue.main.async { NSWorkspace.shared.open(link) }
            }
        }
        labItem.state = labOn ? .on : .off
        m.addItem(labItem)
    }

    /// "Offset" and a slider of ±300 ms: this receiver's sound and picture later (right) or sooner, in a group.
    private func offsetItem(_ r: Receiver) -> NSMenuItem {
        let ms = store.currentOffset(r.id)
        let row = NSView(frame: NSRect(x: 0, y: 0, width: 260, height: 26))
        let label = NSTextField(labelWithString: "Offset")
        label.font = .menuFont(ofSize: 13)
        label.frame = NSRect(x: 22, y: 5, width: 44, height: 16)
        row.addSubview(label)
        let value = NSTextField(labelWithString: Self.ms(ms))
        value.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        value.textColor = .secondaryLabelColor
        value.frame = NSRect(x: 196, y: 5, width: 60, height: 16)
        let slider = NSSlider(value: ms, minValue: -300, maxValue: 300, target: nil, action: nil)
        slider.controlSize = .small
        slider.isContinuous = true
        slider.frame = NSRect(x: 70, y: 4, width: 122, height: 18)
        let action = SliderAction { [weak self, weak value] s in
            let ms = s.doubleValue.rounded()
            value?.stringValue = Self.ms(ms)
            // Sent when let go: every change is a resync heard on the receiver.
            if NSApp.currentEvent?.type == .leftMouseUp { self?.offset(ms, for: r.id) }
        }
        slider.target = action
        slider.action = #selector(SliderAction.changed(_:))
        row.addSubview(slider)
        row.addSubview(value)
        let item = NSMenuItem()
        item.view = row
        item.representedObject = action
        item.toolTip = "This receiver's own correction in a group (positive = later), for its output route"
        return item
    }

    private static func ms(_ v: Double) -> String { String(format: "%+.0f ms", v) }

    /// The Play Together dialog: which receivers, what each presents, the screen or the sound, and the timing.
    private func choose(from receivers: [Receiver]) {
        let alert = NSAlert()
        alert.messageText = "Play Together"
        alert.informativeText = "The receivers play in step. The first one with the picture shows it."
        alert.addButton(withTitle: "Play")
        alert.addButton(withTitle: "Cancel")
        let roles: [(String, GroupRole)] = [("Picture and Sound", .both), ("Picture", .video), ("Sound", .audio)]
        let rowH: CGFloat = 26
        let height = CGFloat(receivers.count + 2) * rowH + 8
        let view = NSView(frame: NSRect(x: 0, y: 0, width: 380, height: height))
        var rows: [(NSButton, NSPopUpButton)] = []
        for (i, r) in receivers.enumerated() {
            let y = height - CGFloat(i + 1) * rowH
            let check = NSButton(checkboxWithTitle: r.name, target: nil, action: nil)
            check.frame = NSRect(x: 0, y: y, width: 200, height: 22)
            let role = NSPopUpButton(frame: NSRect(x: 210, y: y - 2, width: 170, height: 24))
            role.addItems(withTitles: roles.map(\.0))
            role.selectItem(at: i == 0 ? 0 : 2)
            view.addSubview(check)
            view.addSubview(role)
            rows.append((check, role))
        }
        let what = NSPopUpButton(frame: NSRect(x: 0, y: rowH, width: 380, height: 24))
        what.addItems(withTitles: ["This Mac's screen and sound", "This Mac's sound only"])
        let timing = NSPopUpButton(frame: NSRect(x: 0, y: 0, width: 380, height: 24))
        timing.addItems(withTitles: ["Interactive: the picture as fast as alone, the sound waits for it",
                                     "Watching: every receiver at the same delay (rooms, TV walls)"])
        view.addSubview(what)
        view.addSubview(timing)
        alert.accessoryView = view
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }

        let soundOnly = what.indexOfSelectedItem == 1
        var members = zip(receivers, rows).filter { $0.1.0.state == .on }
            .map { GroupMember($0.0, role: roles[$0.1.1.indexOfSelectedItem].1) }
        // The picture's receiver leads (luchd: the first one shows it).
        if let lead = members.firstIndex(where: { $0.role.video }) { members.insert(members.remove(at: lead), at: 0) }
        let mode: SyncMode = timing.indexOfSelectedItem == 0 ? .interactive : .watching
        guard members.count >= 2 else { return show("Tick two or more receivers") }
        if !soundOnly && !members[0].role.video { return show("One receiver must show the picture") }
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            try? self?.play(members, mode: mode, soundOnly: soundOnly)
        }
    }
}

private struct Failure: Error, LocalizedError {
    let errorDescription: String?
    init(_ text: String) { errorDescription = text }
}

/// A slider's action as a closure, kept alive by its menu item.
private final class SliderAction: NSObject {
    private let run: (NSSlider) -> Void
    init(_ run: @escaping (NSSlider) -> Void) { self.run = run }
    @objc func changed(_ sender: NSSlider) { run(sender) }
}
