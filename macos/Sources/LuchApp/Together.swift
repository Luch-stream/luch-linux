// Playing in step on several receivers (PROTOCOL.md 4k), each receiver's sound mode and correction, and the latency
// lab (4j): the local API's `group`, `group_add`, `group_remove`, `offset`, `audio` and `latency` commands, and their
// places in the menu: one "Play on Several TVs…" with three ready choices, and while it plays who is in it with
// Remove, Add for the others, how far behind the screen it is, and Calibrate with Microphone.
// Runs the commands on the local socket's thread (a group waits for every receiver); AppKit only on the main one.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import LuchEngine
import LuchLocal
import LuchNet
import LuchProto
import LuchTV

final class Together {
    static let commands: Set<String> = ["group", "group_add", "group_remove", "group_stats", "offset", "audio", "latency",
                                        "calibrate"]

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
            case "group_add": return try addMember(request)
            case "group_remove": return try removeMember(request)
            case "group_stats": return ["group": currentGroup()?.stats ?? [:]]
            case "offset": return try setOffset(request)
            case "audio": return try setAudio(request)
            case "latency": return try latency(request)
            case "calibrate": return request["self"] as? Bool == true ? try SelfLoop.run() : try calibrate()
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
        // Every member given role audio: music on all of them, as with --source audio.
        try play(members, mode: mode, soundOnly: source == "audio" || !members.contains { $0.role.video })
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
            self.show(Plain.stopped(reason) ?? "")
        }
        show("Connecting to \(members.map(\.receiver.name).joined(separator: ", "))…")
        do {
            try s.start()
        } catch {
            show(Plain.problem(error.localizedDescription) ?? "")
            throw error
        }
        lock.lock()
        group = s
        let labID = lab?.id
        lock.unlock()
        if let labID { s.send(["t": "latency", "on": true], to: labID) }
        show("Playing")
    }

    /// `luchctl group add <r>[=role]`: one more receiver into the group that plays; no role is the group's default.
    private func addMember(_ request: [String: Any]) throws -> [String: Any] {
        guard let g = currentGroup() else { throw Failure("no group is playing: start one with `luchctl group`") }
        let r = try find(request["r"] as? String)
        let text = request["role"] as? String ?? ""
        guard let role = text.isEmpty ? g.defaultJoinRole : GroupRole(rawValue: text) else {
            throw Failure("role video|audio|both, not \(text)")
        }
        if onMain({ hooks.mirroring()?.id == r.id }) { throw Failure("\(r.name) is playing already: stop it first") }
        show("Adding \(r.name)…")
        do {
            try g.add(GroupMember(r, role: role))
        } catch {
            show("")
            throw error
        }
        show("Playing")
        return ["ok": true]
    }

    /// `luchctl group remove <r>`: one out, the others play on; the last one out ends the group.
    private func removeMember(_ request: [String: Any]) throws -> [String: Any] {
        let r = try find(request["r"] as? String)
        guard currentGroup()?.remove(r.id) != nil else { throw Failure("\(r.name) is not playing in a group") }
        onMain(hooks.changed)
        return ["ok": true]
    }

    /// From the menu, off the main thread: a refusal in plain words where the group's status shows.
    private func change(_ run: @escaping () throws -> Void) {
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            do {
                try run()
            } catch {
                self?.show(Plain.problem(error.localizedDescription) ?? "")
            }
            DispatchQueue.main.async { self?.hooks.changed() }
        }
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

    /// "On 3 TVs · All in step, 0.24 s behind this screen".
    static func delayLine(_ g: [String: Any]) -> String {
        let n = (g["members"] as? [[String: Any]] ?? []).count
        return "On \(n) TVs · " + GroupWords.delay(g)
    }

    /// Luch receivers that can play in a group (not TVs without Luch, not a browser).
    private func candidates(_ receivers: [Receiver]) -> [Receiver] {
        receivers.filter { TVKind.of($0.id) == .luch && $0.service != "web" }
    }

    /// The top of the menu while a group plays: what it is doing and Stop, as for a single session. False when none
    /// plays.
    @discardableResult
    func addStatusItems(to menu: NSMenu) -> Bool {
        guard let g = currentGroup() else { return false }
        let item = NSMenuItem.label(Self.delayLine(g.stats["group"] as? [String: Any] ?? [:]))
        statusItem = item
        menu.addItem(item)
        menu.addItem(.run("Stop on All TVs", symbol: "stop.circle") { [weak self] in
            DispatchQueue.global(qos: .userInitiated).async { self?.stop() }
        })
        return true
    }

    /// After the TVs: Play on Several TVs…, or while a group plays its TVs with Remove, Add for the others, and
    /// Calibrate with Microphone.
    func addMenuItems(to menu: NSMenu, receivers: [Receiver]) {
        lock.lock()
        let (g, text) = (group, status)
        lock.unlock()
        guard let g else {
            let can = candidates(receivers)
            menu.addItem(.run("Play on Several TVs…", enabled: can.count >= 2, symbol: "hifispeaker.2") { [weak self] in
                self?.choose(from: can)
            })
            if !text.isEmpty { menu.addItem(.label(text)) }
            return
        }
        let members = (g.stats["group"] as? [String: Any])?["members"] as? [[String: Any]] ?? []
        let names = Dictionary(receivers.map { ($0.id, $0.name) }, uniquingKeysWith: { a, _ in a })
        let playing = g.members
        let parent = NSMenuItem(title: GroupWords.title(playing.map { names[$0.receiver.id] ?? $0.receiver.name }),
                                action: nil, keyEquivalent: "")
        parent.image = NSImage(systemSymbolName: "hifispeaker.2", accessibilityDescription: nil)
        let sub = NSMenu()
        for m in playing {
            let role = members.first { $0["id"] as? String == m.receiver.id }?["role"] as? String ?? m.role.rawValue
            let id = m.receiver.id
            sub.addItem(.run("Remove \(m.receiver.name) (\(GroupWords.role(role)))", symbol: "minus.circle") { [weak self] in
                self?.change { _ = g.remove(id) }
            })
        }
        let others = candidates(receivers).filter { r in !playing.contains { $0.receiver.id == r.id } }
        if !others.isEmpty { sub.addItem(.separator()) }
        for r in others { sub.addItem(addItem(r, to: g)) }
        sub.addItem(.separator())
        sub.addItem(calibrateItem(g))
        parent.submenu = sub
        menu.addItem(parent)
        if !text.isEmpty && text != "Playing" { menu.addItem(.label(text)) }
    }

    private func addItem(_ r: Receiver, to g: GroupSession, title: String? = nil) -> NSMenuItem {
        .run(title ?? "Add \(r.name)", symbol: "plus.circle") { [weak self] in
            self?.change {
                self?.show("Adding \(r.name)…")
                try g.add(GroupMember(r, role: g.defaultJoinRole))
                self?.show("Playing")
            }
        }
    }

    /// Settings ▸ Advanced: the latency lab, a tool for measuring, per receiver.
    func addAdvancedItems(to menu: NSMenu, receivers: [Receiver]) {
        let parent = NSMenuItem(title: "Latency Lab", action: nil, keyEquivalent: "")
        parent.image = NSImage(systemSymbolName: "stopwatch", accessibilityDescription: nil)
        let sub = NSMenu()
        lock.lock()
        let labID = lab?.id
        lock.unlock()
        for r in candidates(receivers) {
            let on = labID == r.id
            let item = NSMenuItem.run(r.name) { [weak self] in
                DispatchQueue.global(qos: .userInitiated).async {
                    guard let url = try? self?.setLab(!on, for: r.id), let link = URL(string: url) else { return }
                    DispatchQueue.main.async { NSWorkspace.shared.open(link) }
                }
            }
            item.state = on ? .on : .off
            sub.addItem(item)
        }
        parent.submenu = sub
        parent.isEnabled = !sub.items.isEmpty
        menu.addItem(parent)
    }

    /// A receiver's own items: in or out of the group that plays, its sound, and its timing in a group.
    func addReceiverItems(to m: NSMenu, receiver r: Receiver) {
        if let g = currentGroup() {
            if g.receiverIDs.contains(r.id) {
                m.addItem(.run("Remove from Several TVs", symbol: "minus.circle") { [weak self] in
                    self?.change { _ = g.remove(r.id) }
                })
                m.addItem(offsetItem(r))
            } else if onMain({ hooks.mirroring()?.id != r.id }) {
                m.addItem(addItem(r, to: g, title: "Add to Several TVs"))
            }
        }
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
    }

    /// "Timing" and a slider of ±300 ms: this receiver's sound and picture later (right) or sooner, in a group.
    private func offsetItem(_ r: Receiver) -> NSMenuItem {
        let ms = store.currentOffset(r.id)
        let row = NSView(frame: NSRect(x: 0, y: 0, width: 260, height: 26))
        let label = NSTextField(labelWithString: "Timing")
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
        item.toolTip = "Later (right) or sooner (left) than the other TVs, when it does not sound together with them"
        return item
    }

    private static func ms(_ v: Double) -> String { String(format: "%+.0f ms", v) }

    /// The Play on Several TVs dialog: one of three ready choices, the TVs (all ticked), the picture's TV for "Screen
    /// Here, Sound There", and the timing under More Options. No roles or modes to understand.
    private func choose(from receivers: [Receiver]) {
        var choice = GroupChoice(receivers.map { ($0.id, $0.name) })
        let alert = NSAlert()
        alert.messageText = "Play on Several TVs"
        alert.informativeText = "Pick what to play and where. The TVs play it at the same moment."
        alert.addButton(withTitle: "Play")
        alert.addButton(withTitle: "Cancel")
        let rowH: CGFloat = 24
        let width: CGFloat = 400
        let height = CGFloat(GroupPreset.allCases.count) * 40 + 24 + CGFloat(receivers.count) * rowH + 70
        let view = NSView(frame: NSRect(x: 0, y: 0, width: width, height: height))
        var y = height
        // Radio buttons of one superview act as one group: the choices apart from the picture's TV.
        let presetBox = NSView(frame: view.bounds)
        view.addSubview(presetBox)
        var presetButtons: [NSButton] = []
        var rows: [(check: NSButton, picture: NSButton)] = []
        let timing = NSPopUpButton(frame: .zero)
        let note = NSTextField(labelWithString: "")
        let actions = Actions()

        func draw() {
            for (b, p) in zip(presetButtons, GroupPreset.allCases) { b.state = p == choice.preset ? .on : .off }
            for (row, r) in zip(rows, receivers) {
                row.check.state = choice.ticked.contains(r.id) ? .on : .off
                row.picture.isHidden = choice.preset != .split || !choice.ticked.contains(r.id)
                row.picture.state = choice.picture == r.id ? .on : .off
            }
            timing.isEnabled = choice.preset != .music
            timing.selectItem(at: (choice.timing ?? choice.preset.mode) == .watching ? 0 : 1)
            if case .failure(let why) = choice.plan() { note.stringValue = why.description } else { note.stringValue = "" }
            alert.buttons.first?.isEnabled = (try? choice.plan().get()) != nil
        }

        for p in GroupPreset.allCases {
            y -= 40
            let b = NSButton(radioButtonWithTitle: p.title, target: actions, action: #selector(Actions.run(_:)))
            b.frame = NSRect(x: 0, y: y + 18, width: width, height: 20)
            actions.add(b) { choice.preset = p; draw() }
            let detail = NSTextField(labelWithString: p.detail)
            detail.font = .systemFont(ofSize: 11)
            detail.textColor = .secondaryLabelColor
            detail.frame = NSRect(x: 20, y: y + 2, width: width - 20, height: 16)
            presetBox.addSubview(b)
            presetBox.addSubview(detail)
            presetButtons.append(b)
        }
        y -= 24
        let which = NSTextField(labelWithString: "On which TVs:")
        which.frame = NSRect(x: 0, y: y + 2, width: width, height: 18)
        view.addSubview(which)
        for r in receivers {
            y -= rowH
            let check = NSButton(checkboxWithTitle: r.name, target: actions, action: #selector(Actions.run(_:)))
            check.frame = NSRect(x: 0, y: y, width: 260, height: 22)
            actions.add(check) { [weak check] in choice.tick(r.id, check?.state == .on); draw() }
            // "Screen Here, Sound There": which TV shows the picture; the others play its sound.
            let picture = NSButton(radioButtonWithTitle: "Picture here", target: actions, action: #selector(Actions.run(_:)))
            picture.frame = NSRect(x: 270, y: y, width: 130, height: 22)
            actions.add(picture) { choice.setPicture(r.id); draw() }
            view.addSubview(check)
            view.addSubview(picture)
            rows.append((check, picture))
        }
        y -= 34
        let more = NSTextField(labelWithString: "More options — when the TVs lag behind each other:")
        more.font = .systemFont(ofSize: 11)
        more.textColor = .secondaryLabelColor
        more.frame = NSRect(x: 0, y: y + 14, width: width, height: 16)
        view.addSubview(more)
        timing.frame = NSRect(x: 0, y: y - 10, width: width, height: 24)
        timing.addItems(withTitles: ["All in step (rooms, a wall of TVs)", "Picture without delay (work, games)"])
        timing.target = actions
        timing.action = #selector(Actions.run(_:))
        actions.add(timing) { [weak timing] in choice.timing = timing?.indexOfSelectedItem == 1 ? .interactive : .watching }
        view.addSubview(timing)
        note.textColor = .systemRed
        note.frame = NSRect(x: 0, y: y - 34, width: width, height: 18)
        view.addSubview(note)
        alert.accessoryView = view
        draw()
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn, let plan = try? choice.plan().get() else { return }
        let byID = Dictionary(receivers.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        let members = plan.members.compactMap { m in byID[m.id].map { GroupMember($0, role: m.role) } }
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            try? self?.play(members, mode: plan.mode, soundOnly: plan.soundOnly)
        }
    }
}

/// The dialog's buttons' actions as closures, kept alive while it is open.
private final class Actions: NSObject {
    private var runs: [ObjectIdentifier: () -> Void] = [:]
    func add(_ control: NSControl, _ run: @escaping () -> Void) { runs[ObjectIdentifier(control)] = run }
    @objc func run(_ sender: NSControl) { runs[ObjectIdentifier(sender)]?() }
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
