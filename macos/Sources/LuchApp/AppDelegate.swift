// Menu bar UI for Luch on macOS: receiver list, mirror/stop, live latency, quality and login item.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import CoreGraphics
import LuchCast
import LuchEngine
import ScreenCaptureKit
import LuchLocal
import LuchMedia
import LuchNet
import LuchProto
import LuchTV
import LuchWeb
import ServiceManagement

final class Preferences {
    static let shared = Preferences()
    private let defaults = UserDefaults.standard

    /// How a receiver gets this Mac's screen; as it is until chosen.
    func screenMode(for id: String) -> ScreenMode {
        (defaults.string(forKey: "screen." + id)).flatMap(ScreenMode.init(rawValue:)) ?? .asIs
    }

    func setScreenMode(_ m: ScreenMode, for id: String) {
        defaults.set(m.rawValue, forKey: "screen." + id)
    }

    var bitrate: Int {
        get { (defaults.object(forKey: "bitrate") as? NSNumber)?.intValue ?? Proto.videoKbps }
        set { defaults.set(newValue, forKey: "bitrate") }
    }

    var audio: Bool {
        get { (defaults.object(forKey: "audio") as? NSNumber)?.boolValue ?? true }
        set { defaults.set(newValue, forKey: "audio") }
    }

    var display: Int? {
        get { (defaults.object(forKey: "display") as? NSNumber)?.intValue }
        set {
            if let newValue { defaults.set(newValue, forKey: "display") }
            else { defaults.removeObject(forKey: "display") }
        }
    }

    var codec: String {
        get { defaults.string(forKey: "codec") ?? CodecChoice.auto.rawValue }
        set { defaults.set(newValue, forKey: "codec") }
    }

    /// Continuous 0...1; the old off/soft/strong strings are migrated on read.
    var warmth: Double {
        get {
            if defaults.object(forKey: "warmthLevel") != nil { return defaults.double(forKey: "warmthLevel") }
            switch defaults.string(forKey: "warmth") ?? "off" {
            case "soft": return 0.35
            case "strong": return 0.7
            default: return 0
            }
        }
        set { defaults.set(min(1, max(0, newValue)), forKey: "warmthLevel") }
    }

    var preset: String {
        get { defaults.string(forKey: "quality") ?? QualityPreset.auto.rawValue }
        set { defaults.set(newValue, forKey: "quality") }
    }

    var sizeMode: String {
        get { defaults.string(forKey: "sizeMode") ?? SizeMode.fit.rawValue }
        set { defaults.set(newValue, forKey: "sizeMode") }
    }

    var mirrorOnLaunch: Bool {
        get { (defaults.object(forKey: "mirrorOnLaunch") as? NSNumber)?.boolValue ?? false }
        set { defaults.set(newValue, forKey: "mirrorOnLaunch") }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem!
    private let menu = NSMenu()
    private let discovery = Discovery()
    /// Receivers past mDNS, the ones talked to before, waking them (Finding.swift).
    private(set) lazy var finding = Finding(discovery: discovery)
    /// Do-not-disturb follows a mirroring session (sound only does not count).
    private(set) var engine: Engine? { didSet { caster.presenter.mirroring(engine.map { !$0.options.soundOnly } ?? false) } }
    /// What is happening now, first in the menu; "Idle" shows nothing.
    private var state = "Idle"
    private var statsLine = ""
    private var statsItem: NSMenuItem?
    /// macOS refused Screen Recording: the menu shows the way to the switch and a restart, which it needs.
    private var needsScreenRecording = false
    private var timer: Timer?
    private(set) var receivers: [Receiver] = []
    private var pendingMenuRefresh = false
    private var menuOpen = false
    private var server: LocalServer?
    private(set) var currentReceiverID: String?
    private var currentReceiver: Receiver?
    private weak var warmPercent: NSTextField?
    /// Set from `luchctl mirror --frame-ms N`, for A/B testing the audio buffer without a rebuild.
    private var frameMsOverride: Int?
    private var profileOverride: VideoProfile?
    /// Codec asked for by `luchctl --codec` for this session only; the menu changes the default.
    private var codecOverride: CodecChoice?
    /// Size mode asked for by `luchctl --size-mode` for this session only.
    private var sizeModeOverride: SizeMode?
    private var presetOverride: QualityPreset?

    func applicationDidFinishLaunching(_ notification: Notification) {
        logFile = defaultLogFile()
        // Where it went before, unbounded.
        try? FileManager.default.removeItem(at: Bundle.main.bundleURL.deletingLastPathComponent()
            .appendingPathComponent("luch-app.log"))
        log("app: launched, bundle \(Bundle.main.bundleURL.lastPathComponent)")
        // A write to a socket the other side has closed (a receiver restarted mid-session, luchctl gone before its
        // answer) must be an error, not SIGPIPE: that ends the whole app silently, with no crash report.
        signal(SIGPIPE, SIG_IGN)
        DispatchQueue.global(qos: .utility).async { ShellSetup.run() }

        NSApp.setActivationPolicy(.accessory)
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        statusItem.button?.image = NSImage(systemSymbolName: "dot.radiowaves.left.and.right",
                                           accessibilityDescription: "Luch")
        statusItem.button?.toolTip = "Luch"
        statusItem.menu = menu
        menu.delegate = self
        setUpShowFiles(statusItem.button)

        discovery.onUpdate = { [weak self] in
            DispatchQueue.main.async { self?.refreshReceivers() }
        }
        discovery.start()
        finding.onChange = { [weak self] in DispatchQueue.main.async { self?.refreshReceivers() } }
        finding.finder.isConnected = { [caster] id in caster.existing(id) != nil }
        finding.direct.direct.isBusy = { [weak self] id in DispatchQueue.main.sync { self?.currentReceiverID == id } }
        caster.onOpen = { [finding] r in finding.finder.remember(r) }
        try? finding.finder.start("auto")
        let identity = senderIdentity()
        discovery.publish(name: identity.name, id: identity.id)
        startLocalServer()
        caster.onChange = { [weak self] in DispatchQueue.main.async { self?.rebuildMenu() } }
        pairPrompt.start()
        pairPrompt.onChange = { [weak self] in self?.rebuildMenu() }
        ReportNotices.shared.start()
        setUpPresenting { [weak self] id in self?.currentReceiverID == id ? self?.engine : nil }
        wireTogether()
        caster.tvs.onChange = { [weak self] in DispatchQueue.main.async { self?.refreshReceivers() } }
        caster.tvs.start()
        startWebReceiver()

        rebuildMenu()
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in self?.tick() }

        if Preferences.shared.mirrorOnLaunch {
            DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in self?.mirrorIfSingle() }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        engine?.stop(reason: "quit")
        _ = try? finding.direct.direct.off() // back to the Wi-Fi network a direct connection left
    }

    // MARK: - local API (luchctl)

    let caster = Caster()
    let pairPrompt = PairPrompt()
    let together = Together()

    private func startLocalServer() {
        let server = LocalServer()
        server.handler = { [weak self] request in
            guard let self else { return ["error": "app is shutting down"] }
            if PairingCommands.commands.contains(request["cmd"] as? String ?? "") {
                var known: [Receiver] = []
                DispatchQueue.main.sync { known = self.receivers }
                return PairingCommands.handle(request, receivers: known)
            }
            if let answer = self.together.intercept(request) { return answer }
            if Self.reportCommands.contains(request["cmd"] as? String ?? "") { return self.handleReport(request) }
            // What waits for a receiver's answer runs here, on the socket's thread, not on the main one.
            if WebReceiver.commands.contains(request["cmd"] as? String ?? "") { return webReceiver.handle(request) }
            if Caster.commands.contains(request["cmd"] as? String ?? "") {
                var known: [Receiver] = []
                var mirroring: String?
                DispatchQueue.main.sync {
                    known = self.receivers
                    mirroring = self.currentReceiverID
                }
                return self.caster.handle(request, receivers: known, mirroring: mirroring)
            }
            if Finding.commands.contains(request["cmd"] as? String ?? "") {
                var listed: [Receiver] = []
                DispatchQueue.main.sync { listed = self.receivers }
                return self.finding.handle(request, receivers: listed)
            }
            var response: [String: Any] = [:]
            // The socket thread must not touch AppKit state directly.
            DispatchQueue.main.sync { response = self.handleLocal(request) }
            return response
        }
        do {
            try server.start()
            self.server = server
        } catch {
            log("app: local server failed: \(error.localizedDescription)")
        }
    }

    private func handleLocal(_ request: [String: Any]) -> [String: Any] {
        switch request["cmd"] as? String ?? "" {
        case "ping":
            return ["ok": true]
        case "receivers":
            try? finding.finder.start("auto") // as luchd's list: the automatic ladder, at most every 30 s
            return ["receivers": receivers.map { receiver in
                ["id": receiver.id, "name": receiver.name, "model": receiver.model,
                 "address": receiver.address, "iface": receiver.interfaceName,
                 "port": Int(receiver.port), "kind": TVKind.of(receiver.id).rawValue,
                 "mirroring": engine != nil && currentReceiverID == receiver.id]
            }]
        case "state":
            return ["state": state, "streaming": engine != nil, "stats": statsDictionary()]
        case "stats":
            return ["stats": statsDictionary()]
        case "mirror":
            if let frameMs = (request["frame_ms"] as? NSNumber)?.intValue { frameMsOverride = frameMs }
        if let codec = request["codec"] as? String, let value = CodecChoice(rawValue: codec) {
            codecOverride = value
        }
        if let mode = request["size_mode"] as? String, let value = SizeMode(rawValue: mode) {
            sizeModeOverride = value
        }
        if let preset = request["preset"] as? String, let value = QualityPreset.parse(preset) {
            presetOverride = value
        }
        if let profile = request["profile"] as? String, let value = VideoProfile(rawValue: profile) {
            self.profileOverride = value
        }
            let wanted = (request["id"] as? String) ?? (request["name"] as? String)
            let match = receivers.first { $0.id == wanted || $0.name == wanted }
                ?? (receivers.count == 1 ? receivers[0] : nil)
            guard let receiver = match else { return ["error": "receiver not found"] }
            if TVKind.of(receiver.id) != .luch {
                return ["error": "\(receiver.name) has no Luch app: it plays files and links (luchctl cast), not this screen"]
            }
            if let word = request["source"] as? String {
                guard let source = MirrorSource.parse(word, which: request["which"] as? String) else {
                    return ["error": "mirror: fast|monitor|window|virtual|audio or asis|extend|speed|quality|mac"]
                }
                return mirror(source, on: receiver)
            }
            let ultra = (request["ultra"] as? String).flatMap(UltraPriority.init(rawValue:))
            if request["ultra"] != nil, ultra == nil { return ["error": "ultra: speed, quality or mac"] }
            startMirror(receiver, extend: request["extend"] as? Bool, ultra: ultra)
            return ["ok": true, "receiver": receiver.name]
        case "screen":
            guard let mode = (request["mode"] as? String).flatMap(ScreenMode.init(rawValue:)) else {
                return ["error": "screen: asis, speed, quality, mac or extend"]
            }
            guard let id = currentReceiverID, let r = receivers.first(where: { $0.id == id }) else {
                return ["error": "not mirroring"]
            }
            Preferences.shared.setScreenMode(mode, for: r.id)
            switchScreen(mode, on: r)
            return ["ok": true, "receiver": r.name]
        case "warmth":
            let value: Double
            if let number = request["value"] as? NSNumber { value = number.doubleValue }
            else if let text = request["value"] as? String, let parsed = Double(text) { value = parsed }
            else { return ["error": "warmth expects a number 0...1"] }
            let warmth = min(1, max(0, value))
            Preferences.shared.warmth = warmth
            engine?.warmth = warmth     // live: the next frame is already warmer
            rebuildMenu()
            return ["ok": true, "warmth": warmth]
        case "stop":
            stopMirror()
            return ["ok": true]
        case "displays", "windows":
            return sources(request["cmd"] as? String ?? "")
        default:
            return ["error": "unknown command"]
        }
    }

    private func statsDictionary() -> [String: Any] {
        guard let stats = engine?.lastStats else { return [:] }
        var out: [String: Any] = ["est_ms": stats.estMs, "host_ms": stats.hostMs,
                                  "rtt_ms": stats.rttMs, "fps": stats.fps, "mbps": stats.mbps,
                                  "audio": stats.audioPackets]
        if let decode = stats.decodeMs { out["decode_ms"] = decode }
        return out
    }

    // MARK: - menu

    func refreshReceivers() {
        receivers = caster.rows(finding.finder.list()) + webReceiver.receivers
        rebuildMenu()
    }

    private func rebuildMenu() {
        // Rebuilt under the pointer, an open menu would close its submenus: wait for it to close.
        if menuOpen {
            pendingMenuRefresh = true
            return
        }
        menu.removeAllItems()

        // What is happening now, and the way to stop it, before anything else.
        let busy = engine != nil || state != "Idle"
        if busy { menu.addItem(.label(state)) }
        statsItem = nil
        if engine != nil {
            if !statsLine.isEmpty {
                let stats = NSMenuItem.label(statsLine)
                statsItem = stats
                menu.addItem(stats)
            }
            menu.addItem(.run("Stop Mirroring", symbol: "stop.circle") { [weak self] in self?.stopMirror() })
        }
        if needsScreenRecording { screenRecordingItems().forEach(menu.addItem) }
        if busy || needsScreenRecording { menu.addItem(.separator()) }

        // The first time: where a TV comes from, and the harder search right here, not two menus down.
        if receivers.isEmpty {
            menu.addItem(.label("No receivers found yet"))
            menu.addItem(.label("Open Luch on the TV: it shows up here"))
            menu.addItem(finding.searchHarderItem())
        }
        for receiver in receivers {
            let item = NSMenuItem(title: receiver.name, action: nil, keyEquivalent: "")
            let luch = TVKind.of(receiver.id) == .luch
            item.submenu = receiver.service == "web" ? webBrowserMenu(receiver) : luch ? receiverMenu(receiver) : tvMenu(receiver)
            if caster.existing(receiver.id)?.showing != nil || currentReceiverID == receiver.id
                || caster.tvs.session(receiver.id) != nil { item.state = .on }
            item.image = NSImage(systemSymbolName: receiver.service == "web" ? "globe" : "tv", accessibilityDescription: nil)
            menu.addItem(item)
        }
        finding.addMenu(to: menu, online: receivers)
        menu.addItem(webMenuItem())

        menu.addItem(.separator())
        together.addMenuItems(to: menu, receivers: receivers)
        menu.addItem(.separator())

        menu.addItem(settingsItem())
        menu.addItem(.separator())
        menu.addItem(reportMenuItem())
        let quit = NSMenuItem(title: "Quit Luch", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        menu.addItem(quit)
    }

    /// The picture and sound, how the app starts, and the expert's knobs, out of the first level.
    private func settingsItem() -> NSMenuItem {
        let parent = NSMenuItem(title: "Settings", action: nil, keyEquivalent: "")
        parent.image = NSImage(systemSymbolName: "gearshape", accessibilityDescription: nil)
        let settings = NSMenu()

        let audio = NSMenuItem(title: "Send This Mac's Sound", action: #selector(toggleAudio), keyEquivalent: "")
        audio.target = self
        audio.state = Preferences.shared.audio ? .on : .off
        settings.addItem(audio)

        // One row: label, slider, percentage. A bare NSSlider as the item view sits crooked in a menu.
        let row = NSView(frame: NSRect(x: 0, y: 0, width: 260, height: 26))
        let label = NSTextField(labelWithString: "Warm")
        label.font = .menuFont(ofSize: 13)
        label.frame = NSRect(x: 22, y: 5, width: 44, height: 16)
        row.addSubview(label)

        let slider = NSSlider(value: Preferences.shared.warmth, minValue: 0, maxValue: 1, target: self,
                              action: #selector(warmthChanged(_:)))
        slider.isContinuous = true
        slider.controlSize = .small
        slider.frame = NSRect(x: 70, y: 4, width: 148, height: 18)
        row.addSubview(slider)

        let percent = NSTextField(labelWithString: String(format: "%d%%", Int((Preferences.shared.warmth * 100).rounded())))
        percent.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        percent.textColor = .secondaryLabelColor
        percent.frame = NSRect(x: 222, y: 5, width: 32, height: 16)
        row.addSubview(percent)
        warmPercent = percent

        let warmItem = NSMenuItem()
        warmItem.view = row
        warmItem.toolTip = "A warmer picture on the TV in the evening, as Night Shift"
        settings.addItem(warmItem)

        // Warn about the cost, do not silently change the user's choice.
        let onBattery = engine?.deviceOnBattery ?? false
        settings.addItem(choices("Quality", QualityPreset.allCases.map {
            ($0.rawValue, $0.label + (onBattery && $0 != .low ? " — drains the battery faster" : ""))
        }, chosen: Preferences.shared.preset, #selector(setPreset(_:))))
        settings.addItem(choices("Picture Fit", SizeMode.allCases.map { ($0.rawValue, $0.label) },
                                 chosen: Preferences.shared.sizeMode, #selector(setSizeMode(_:))))

        let display = NSMenuItem(title: "Display", action: nil, keyEquivalent: "")
        let displayMenu = NSMenu()
        let main = NSMenuItem(title: "Main display", action: #selector(setDisplay(_:)), keyEquivalent: "")
        main.target = self
        main.representedObject = -1
        main.state = Preferences.shared.display == nil ? .on : .off
        displayMenu.addItem(main)
        for (index, screen) in NSScreen.screens.enumerated() {
            let item = NSMenuItem(title: screen.localizedName, action: #selector(setDisplay(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = index
            item.state = Preferences.shared.display == index ? .on : .off
            displayMenu.addItem(item)
        }
        display.submenu = displayMenu
        settings.addItem(display)

        settings.addItem(.separator())
        settings.addItem(quietItem())
        let launch = NSMenuItem(title: "Mirror When Luch Starts", action: #selector(toggleMirrorOnLaunch), keyEquivalent: "")
        launch.target = self
        launch.state = Preferences.shared.mirrorOnLaunch ? .on : .off
        launch.toolTip = "To the only TV there is, a few seconds after Luch starts"
        settings.addItem(launch)
        let login = NSMenuItem(title: "Start at Login", action: #selector(toggleLogin), keyEquivalent: "")
        login.target = self
        login.state = SMAppService.mainApp.status == .enabled ? .on : .off
        settings.addItem(login)

        settings.addItem(.separator())
        settings.addItem(.label("Advanced"))
        let codecs = CodecChoice.allCases.map { c in
            (c.rawValue, c == .auto ? "Auto" : c == .h264 ? "H.264" : c.rawValue.uppercased())
        }
        settings.addItem(choices("Codec", codecs, chosen: Preferences.shared.codec, #selector(setCodec(_:))))
        let bitrate = NSMenuItem(title: "Bitrate", action: nil, keyEquivalent: "")
        let bitrateMenu = NSMenu()
        for kbps in [10_000, 20_000, 30_000] {
            let item = NSMenuItem(title: "\(kbps / 1000) Mbit/s", action: #selector(setBitrate(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = kbps
            item.state = Preferences.shared.bitrate == kbps ? .on : .off
            bitrateMenu.addItem(item)
        }
        bitrate.submenu = bitrateMenu
        settings.addItem(bitrate)

        parent.submenu = settings
        return parent
    }

    /// A submenu of choices sent to `action` with their raw value; the one in use is checked.
    private func choices(_ title: String, _ options: [(String, String)], chosen: String, _ action: Selector) -> NSMenuItem {
        let parent = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        let m = NSMenu()
        for (raw, label) in options {
            let item = NSMenuItem(title: label, action: action, keyEquivalent: "")
            item.target = self
            item.representedObject = raw
            item.state = chosen == raw ? .on : .off
            m.addItem(item)
        }
        parent.submenu = m
        return parent
    }

    /// Once refused, macOS asks no more: the switch in System Settings, then a restart for the app to see it.
    private func screenRecordingItems() -> [NSMenuItem] {
        [.run("Open Screen Recording Settings…", symbol: "lock.shield") {
            if let pane = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") {
                NSWorkspace.shared.open(pane)
            }
        },
         .run("Restart Luch", symbol: "arrow.clockwise") {
            let relaunch = Process()
            relaunch.executableURL = URL(fileURLWithPath: "/bin/sh")
            relaunch.arguments = ["-c", "sleep 1; /usr/bin/open \"$0\"", Bundle.main.bundlePath]
            try? relaunch.run()
            NSApp.terminate(nil)
        }]
    }

    private func tick() {
        guard let stats = engine?.lastStats else {
            if engine == nil && !statsLine.isEmpty {
                statsLine = ""
                statsItem?.title = statsLine
            }
            return
        }
        apply(stats)
    }

    // MARK: - actions

    @objc private func mirrorIfSingle() {
        guard engine == nil, receivers.count == 1, TVKind.of(receivers[0].id) == .luch else { return }
        startMirror(receivers[0])
    }

    func startMirror(_ receiver: Receiver, soundOnly: Bool = false, extend: Bool? = nil,
                     ultra: UltraPriority? = nil, window: CGWindowID? = nil, displayID: CGDirectDisplayID? = nil) {
        // Nothing asked for: the way chosen for this TV in its menu (also for mirror on launch and luchctl).
        let saved = Preferences.shared.screenMode(for: receiver.id)
        let ultra = ultra ?? (extend == nil && !soundOnly ? saved.ultra : nil)
        let extend = extend ?? (!soundOnly && ultra == nil && saved == .extend)
        // Switching sessions: let the receiver close the previous one first, otherwise it tears
        // down the new session a few hundred ms after start.
        if let old = engine {
            engine = nil
            currentReceiverID = nil
            state = "Switching…"
            rebuildMenu()
            DispatchQueue.global(qos: .userInitiated).async {
                old.stop(reason: "switch")
                Thread.sleep(forTimeInterval: 0.8)
                DispatchQueue.main.async { [weak self] in
                    self?.beginMirror(receiver, soundOnly: soundOnly, extend: extend, ultra: ultra, window: window,
                                      displayID: displayID)
                }
            }
            return
        }
        beginMirror(receiver, soundOnly: soundOnly, extend: extend, ultra: ultra, window: window, displayID: displayID)
    }

    private func beginMirror(_ receiver: Receiver, soundOnly: Bool, extend: Bool, ultra: UltraPriority?,
                             window: CGWindowID?, displayID: CGDirectDisplayID?) {
        if !CGPreflightScreenCaptureAccess() {
            // The system dialog only comes forward for a regular, active app; a background
            // accessory app gets an immediate "declined" with no window at all.
            NSApp.setActivationPolicy(.regular)
            NSApp.activate(ignoringOtherApps: true)
            RunLoop.current.run(until: Date().addingTimeInterval(0.4))
            _ = CGRequestScreenCaptureAccess()
            if !CGPreflightScreenCaptureAccess() {
                // Second system path: ScreenCaptureKit itself triggers the prompt on modern macOS.
                let ready = DispatchSemaphore(value: 0)
                SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { _, _ in
                    ready.signal()
                }
                _ = ready.wait(timeout: .now() + 5)
            }
            NSApp.setActivationPolicy(.accessory)
            if !CGPreflightScreenCaptureAccess() {
                // Once refused, macOS shows no dialog again: only the switch in Settings can allow it, and the
                // app sees the change only after it restarts.
                state = "Luch may not record the screen yet: allow it in System Settings, then restart Luch"
                needsScreenRecording = true
                rebuildMenu()
                if let pane = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") {
                    NSWorkspace.shared.open(pane)
                }
                return
            }
        }

        stopMirror()
        DispatchQueue.global(qos: .userInitiated).async { [together] in together.stop() }
        var options = MirrorOptions()
        options.bitrate = Preferences.shared.bitrate
        options.audioMode = SyncStore.shared.audioMode(receiver.id)
        options.audio = Preferences.shared.audio && options.audioMode != .off
        options.display = Preferences.shared.display
        options.codec = codecOverride ?? (CodecChoice(rawValue: Preferences.shared.codec) ?? .auto)
        options.sizeMode = sizeModeOverride ?? (SizeMode(rawValue: Preferences.shared.sizeMode) ?? .fit)
        options.preset = presetOverride ?? (QualityPreset.parse(Preferences.shared.preset) ?? .balanced)
        options.warmth = Preferences.shared.warmth
        if let frameMs = frameMsOverride { options.audioFrameMs = frameMs }
        options.profile = profileOverride
        options.soundOnly = soundOnly
        options.extend = extend
        options.ultra = ultra
        options.window = window
        options.displayID = displayID ?? (extend || ultra != nil || window != nil ? nil : self.displayID(for: receiver))
        let engine = Engine(options: options)
        if !soundOnly { engine.input = inputControl(for: receiver) }
        engine.shareArt = { [caster] file, client, me in caster.shareArt(file, client: client, me: me) }
        engine.onWaitingForAllow = { [weak self] in
            DispatchQueue.main.async {
                self?.state = "Press Allow on \(receiver.name): left, then OK on its remote"
                self?.rebuildMenu()
            }
        }
        engine.onStats = { [weak self] stats in
            DispatchQueue.main.async { self?.apply(stats) }
        }
        engine.onStopped = { [weak self] reason in
            DispatchQueue.main.async {
                guard let self, self.engine == nil else { return } // replaced by a newer session
                self.currentReceiverID = nil
                self.state = Plain.stopped(reason) ?? "Idle"
                self.statsLine = ""
                self.rebuildMenu()
            }
        }
        self.engine = engine
        currentReceiverID = receiver.id
        currentReceiver = receiver
        state = extend ? "Connecting to \(receiver.name)… the first time, choose Extend in the macOS dialog"
            : "Connecting to \(receiver.name)…"
        rebuildMenu()

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            do {
                try engine.start(receiver: receiver)
                self?.together.sessionStarted(receiver.id)
                self?.finding.finder.remember(receiver)
                let codec = engine.plan?.codec.label ?? ""
                let batteryNote = engine.deviceOnBattery ? " · 🔋 on battery" : ""
                DispatchQueue.main.async {
                    self?.state = soundOnly ? "Playing sound on \(receiver.name)"
                        : extend ? "\(receiver.name) is a second screen · \(codec)"
                        : ultra != nil ? "Mirroring to \(receiver.name) at its \(engine.ultraMode.map { "\($0)" } ?? "") · \(codec)"
                        : window != nil ? "Showing a window on \(receiver.name) · \(codec)"
                        : "Mirroring to \(receiver.name) · \(codec)\(batteryNote)"
                    self?.rebuildMenu()
                }
            } catch {
                DispatchQueue.main.async {
                    // The TV shows a code: the window for it comes with the pairing, and the mirror starts after.
                    if case ConnectError.pairingNeeded = error {
                        self?.pairPrompt.after(receiver.id) {
                            self?.startMirror(receiver, soundOnly: soundOnly, extend: extend, ultra: ultra)
                        }
                        self?.state = "Pairing with \(receiver.name): type the code the TV shows"
                    } else {
                        self?.state = Plain.problem(error.localizedDescription).map { "\(receiver.name): \($0)" } ?? "Idle"
                    }
                    self?.engine = nil
                    self?.rebuildMenu()
                }
            }
        }
    }

    private func apply(_ stats: SessionStats) {
        let (line, details) = Plain.session(stats)
        let first = statsLine.isEmpty
        statsLine = line
        statsItem?.title = line
        statsItem?.toolTip = details
        if first { rebuildMenu() } // the line appears with the first numbers
    }

    @objc func stopMirror() {
        guard let engine else { return }
        self.engine = nil
        currentReceiverID = nil
        currentReceiver = nil
        state = "Stopping…"
        statsLine = ""
        rebuildMenu()
        DispatchQueue.global(qos: .userInitiated).async {
            engine.stop(reason: "menu")
            DispatchQueue.main.async { [weak self] in
                self?.state = "Idle"
                self?.rebuildMenu()
            }
        }
    }

    @objc private func toggleAudio() {
        Preferences.shared.audio.toggle()
        log("app: menu audio -> \(Preferences.shared.audio)")
        restartIfStreaming()
    }

    /// Colour temperature applies live: no restart, no menu rebuild — the next frame is already warmer.
    @objc private func warmthChanged(_ sender: NSSlider) {
        let warmth = min(1, max(0, sender.doubleValue))
        Preferences.shared.warmth = warmth
        engine?.warmth = warmth
        warmPercent?.stringValue = String(format: "%d%%", Int((warmth * 100).rounded()))
    }

    @objc private func setPreset(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String else { return }
        log("app: menu quality -> \(raw)")
        presetOverride = nil
        Preferences.shared.preset = raw
        restartIfStreaming()
    }

    @objc private func setSizeMode(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String else { return }
        log("app: menu size -> \(raw)")
        sizeModeOverride = nil
        Preferences.shared.sizeMode = raw
        restartIfStreaming()
    }

    @objc private func setCodec(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String else { return }
        log("app: menu codec -> \(raw)")
        codecOverride = nil
        Preferences.shared.codec = raw
        restartIfStreaming()
    }

    @objc private func setBitrate(_ sender: NSMenuItem) {
        guard let kbps = sender.representedObject as? Int else { return }
        log("app: menu bitrate -> \(kbps)")
        Preferences.shared.bitrate = kbps
        restartIfStreaming()
    }

    @objc private func setDisplay(_ sender: NSMenuItem) {
        guard let index = sender.representedObject as? Int else { return }
        log("app: menu display -> \(index)")
        Preferences.shared.display = index < 0 ? nil : index
        restartIfStreaming()
    }

    /// A setting changed while streaming: apply it now instead of waiting for the next Mirror.
    private func restartIfStreaming() {
        guard let receiver = currentReceiver, engine != nil else {
            rebuildMenu()
            return
        }
        log("app: setting changed while streaming, restarting (engine=\(engine != nil), receiver=\(receiver.name))")
        // A window shown stays shown.
        if let window = engine?.options.window { startMirror(receiver, extend: false, window: window) }
        else { startMirror(receiver) }
    }

    @objc private func toggleMirrorOnLaunch() {
        Preferences.shared.mirrorOnLaunch.toggle()
        rebuildMenu()
    }

    @objc private func toggleLogin() {
        let service = SMAppService.mainApp
        do {
            if service.status == .enabled {
                try service.unregister()
            } else {
                try service.register()
            }
        } catch {
            state = "Could not change Start at Login: \(error.localizedDescription)"
            log("app: login item failed: \(error.localizedDescription)")
        }
        rebuildMenu()
    }
}

// MARK: - a receiver's menu: everything luchctl does, for the mouse

extension AppDelegate {
    fileprivate func receiverMenu(_ r: Receiver) -> NSMenu {
        let m = NSMenu()
        let mirroringHere = currentReceiverID == r.id
        let mode = Preferences.shared.screenMode(for: r.id)
        m.addItem(.run(mode == .extend ? "Use as Second Screen" : "Mirror Screen", symbol: mode.symbol) { [weak self] in
            self?.startMirror(r, extend: mode == .extend, ultra: mode.ultra)
        })
        let modeItem = NSMenuItem(title: "Show As: \(mode.short)", action: nil, keyEquivalent: "")
        let modeMenu = NSMenu()
        for each in ScreenMode.allCases {
            if each == .extend { modeMenu.addItem(.separator()) }
            let item = NSMenuItem.run(each.label) { [weak self] in
                Preferences.shared.setScreenMode(each, for: r.id)
                self?.switchScreen(each, on: r)
            }
            if each == mode { item.state = .on }
            modeMenu.addItem(item)
        }
        addScreenSources(to: modeMenu, for: r)
        modeItem.submenu = modeMenu
        m.addItem(modeItem)
        m.addItem(.run("Sound Only (as a Speaker)", symbol: "hifispeaker") { [weak self] in self?.startMirror(r, soundOnly: true) })
        if mirroringHere {
            m.addItem(.run("Stop Mirroring", symbol: "stop.circle") { [weak self] in self?.stopMirror() })
        }
        inputItems(for: r).forEach(m.addItem)
        together.addReceiverItems(to: m, receiver: r)
        finding.direct.items(for: r).forEach(m.addItem)

        m.addItem(.separator())
        m.addItem(.run("Show File…", symbol: "doc") { [weak self] in self?.chooseFiles(for: r) })
        let link = Self.clipboardLink()
        let linkTitle = link.flatMap { URL(string: $0)?.host }.map { "Show Link from Clipboard (\($0))" }
            ?? "Show Link from Clipboard"
        m.addItem(.run(linkTitle, enabled: link != nil, symbol: "link") { [weak self] in
            if let link { self?.send(["cmd": "cast", "items": [link]], to: r) }
        })
        presentingItems(r, mirroringHere: mirroringHere).forEach(m.addItem)

        if let s = caster.existing(r.id), let kind = s.showing {
            m.addItem(.separator())
            addShowing(kind, session: s, to: m, receiver: r)
        }

        m.addItem(.separator())
        m.addItem(volumeItem(r))
        m.addItem(.run("Turn Off the TV", symbol: "power") { [weak self] in self?.send(["cmd": "sleep"], to: r) })
        m.addItem(.separator())
        pairPrompt.menuItems(for: r) { [weak self] in self?.send(["cmd": "connect"], to: r) }.forEach(m.addItem)
        return m
    }

    /// The controls for what is shown: a video's, a document's pages, or just Stop.
    private func addShowing(_ kind: String, session s: CastSession, to m: NSMenu, receiver r: Receiver) {
        let state = s.state
        switch kind {
        case "media":
            let title = (state["title"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "Video"
            let playing = state["state"] as? String == "playing"
            var line = title
            if let pos = (state["pos"] as? NSNumber)?.doubleValue {
                line += " · " + Self.clock(pos)
                if let dur = (state["dur"] as? NSNumber)?.doubleValue { line += " / " + Self.clock(dur) }
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
            let speed = NSMenuItem(title: "Speed", action: nil, keyEquivalent: "")
            let speeds = NSMenu()
            let now = (state["rate"] as? NSNumber)?.doubleValue ?? 1
            for rate in [0.75, 1, 1.25, 1.5, 2] {
                let item = NSMenuItem.run(rate == 1 ? "Normal" : String(format: "%g×", rate)) { [weak self] in
                    self?.send(["cmd": "media_cmd", "media": ["do": "rate", "rate": rate]], to: r)
                }
                if abs(rate - now) < 0.01 { item.state = .on }
                speeds.addItem(item)
            }
            speed.submenu = speeds
            m.addItem(speed)
        case "doc":
            let p = s.pages
            m.addItem(.label(p.pages > 0 ? "Document · page \(p.page + 1) of \(p.pages)" : "Document"))
            m.addItem(.run("Previous Page", enabled: p.page > 0, symbol: "chevron.left") { [weak self] in
                self?.send(["cmd": "page", "what": "prev"], to: r)
            })
            m.addItem(.run("Next Page", enabled: p.page + 1 < p.pages, symbol: "chevron.right") { [weak self] in
                self?.send(["cmd": "page", "what": "next"], to: r)
            })
        case "photo":
            m.addItem(.label("Photos · left and right on the remote"))
        default:
            m.addItem(.label("Opened in the TV's own app"))
        }
        m.addItem(.run("Stop Showing", symbol: "xmark.circle") { [weak self] in self?.send(["cmd": "stop_cast"], to: r) })
    }

    /// Volume: a slider (sent when let go), a step either way and mute; the level the TV last said.
    private func volumeItem(_ r: Receiver) -> NSMenuItem {
        let parent = NSMenuItem(title: "Volume", action: nil, keyEquivalent: "")
        parent.image = NSImage(systemSymbolName: "speaker.wave.2", accessibilityDescription: nil)
        let menu = NSMenu()
        let v = caster.existing(r.id)?.currentVolume
        let level = (v?["level"] as? NSNumber)?.intValue
        let muted = (v?["muted"] as? NSNumber)?.boolValue ?? false
        if let level { parent.title = muted ? "Volume · muted" : "Volume · \(level)%" }

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
        let mute = NSMenuItem.run(muted ? "Unmute" : "Mute", symbol: muted ? "speaker.wave.2" : "speaker.slash") { [weak self] in
            self?.send(["cmd": "volume", "what": muted ? "unmute" : "mute"], to: r)
        }
        menu.addItem(mute)
        if level == nil {
            menu.addItem(.separator())
            menu.addItem(.label("The level shows once the TV has been talked to"))
        }
        parent.submenu = menu
        return parent
    }

    /// Off the main thread (it waits for the TV); a refusal is shown, not swallowed.
    func send(_ request: [String: Any], to r: Receiver) {
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let response = self.caster.run(request, on: r)
            DispatchQueue.main.async {
                if let error = response["error"] as? String, error.contains("asks for pairing") {
                    // The window for the code comes with the pairing; this is done again once paired.
                    self.pairPrompt.after(r.id) { self.send(request, to: r) }
                } else if let error = response["error"] as? String, let words = Plain.problem(error) {
                    let alert = NSAlert()
                    alert.messageText = r.name
                    alert.informativeText = words
                    NSApp.activate(ignoringOtherApps: true)
                    alert.runModal()
                }
                self.rebuildMenu()
            }
        }
    }

    func chooseFiles(for r: Receiver) {
        let panel = NSOpenPanel()
        panel.title = "Show on \(r.name)"
        panel.prompt = "Show"
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        NSApp.activate(ignoringOtherApps: true)
        guard panel.runModal() == .OK, !panel.urls.isEmpty else { return }
        send(["cmd": "cast", "items": panel.urls.map(\.path)], to: r)
    }

    /// An http(s) link on the clipboard, if that is what is there.
    static func clipboardLink() -> String? {
        guard let text = NSPasteboard.general.string(forType: .string)?.trimmingCharacters(in: .whitespacesAndNewlines),
              !text.contains(" "), !text.contains("\n"),
              let u = URL(string: text), ["http", "https"].contains(u.scheme?.lowercased() ?? ""), u.host != nil else {
            return nil
        }
        return text
    }

    static func clock(_ seconds: Double) -> String {
        let t = Int(seconds.rounded(.down))
        return t >= 3600 ? String(format: "%d:%02d:%02d", t / 3600, t / 60 % 60, t % 60)
            : String(format: "%d:%02d", t / 60, t % 60)
    }
}

extension AppDelegate: NSMenuDelegate {
    /// Built afresh just before it shows: the clipboard's link, the volume and what plays are as they are now.
    func menuNeedsUpdate(_ menu: NSMenu) {
        guard menu === self.menu, !menuOpen else { return }
        pendingMenuRefresh = false
        rebuildMenu()
    }

    func menuWillOpen(_ menu: NSMenu) {
        guard menu === self.menu else { return }
        menuOpen = true
    }

    func menuDidClose(_ menu: NSMenu) {
        guard menu === self.menu else { return }
        menuOpen = false
        // A notice (why it stopped, what went wrong) is read once, then the menu is back to the list.
        if engine == nil, !needsScreenRecording, state != "Idle" {
            state = "Idle"
            pendingMenuRefresh = true
        }
        if pendingMenuRefresh {
            pendingMenuRefresh = false
            rebuildMenu()
        }
    }
}

// MARK: - how the screen reaches a receiver

extension AppDelegate {
    /// A new way to show the screen takes effect at once when this TV is being mirrored to (sound only stays).
    /// `window`: that window alone instead of the screen.
    func switchScreen(_ mode: ScreenMode, on r: Receiver, window: CGWindowID? = nil) {
        guard let engine, currentReceiverID == r.id, engine.streaming, !engine.options.soundOnly else { return }
        state = "Switching \(r.name) to \(window == nil ? mode.short : "one window")…"
        rebuildMenu()
        let display = mode == .asIs && window == nil ? displayID(for: r) : nil
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            do {
                try engine.switchScreen(extend: mode == .extend, ultra: mode.ultra, window: window, displayID: display)
                let codec = engine.plan?.codec.label ?? ""
                let at = engine.ultraMode.map { " at its \($0)" } ?? ""
                DispatchQueue.main.async {
                    self?.state = (mode == .extend ? "\(r.name) is a second screen" : "Mirroring to \(r.name)\(at)") + " · \(codec)"
                    self?.rebuildMenu()
                }
            } catch {
                DispatchQueue.main.async {
                    self?.state = Plain.problem(error.localizedDescription).map { "\(r.name): \($0)" } ?? "Idle"
                    self?.rebuildMenu()
                }
                engine.stop(reason: "switch failed")
            }
        }
    }
}

/// The receiver menu's choice of how this Mac's screen is shown, kept per TV.
enum ScreenMode: String, CaseIterable {
    /// This Mac's screen as it is, scaled to the stream: nothing changes on the Mac.
    case asIs = "asis"
    /// The desktop drawn at one of the TV's own modes (this Mac's screen shows a copy with bars): nothing is scaled.
    case speed, quality, likeMac = "mac"
    /// A second screen of the TV's size beside this Mac's.
    case extend

    var ultra: UltraPriority? {
        switch self {
        case .speed: return .speed
        case .quality: return .quality
        case .likeMac: return .likeMac
        case .asIs, .extend: return nil
        }
    }

    var label: String {
        switch self {
        case .asIs: return "As It Is: This Mac's Screen, Scaled"
        case .speed: return "TV's Own Size · Smoothest (1080p at its fastest rate)"
        case .quality: return "TV's Own Size · Sharpest"
        case .likeMac: return "TV's Own Size · Closest to This Mac"
        case .extend: return "As a Second Screen"
        }
    }

    var short: String {
        switch self {
        case .asIs: return "As It Is"
        case .speed: return "TV's Size, Smoothest"
        case .quality: return "TV's Size, Sharpest"
        case .likeMac: return "TV's Size, Like This Mac"
        case .extend: return "Second Screen"
        }
    }

    var symbol: String {
        switch self {
        case .asIs: return "rectangle.on.rectangle"
        case .speed, .quality, .likeMac: return "bolt"
        case .extend: return "rectangle.split.2x1"
        }
    }
}

// MARK: - playing in step, sound modes, the latency lab (Together.swift)

extension AppDelegate {
    fileprivate func wireTogether() {
        together.hooks.receivers = { [weak self] in self?.receivers ?? [] }
        together.hooks.mirroring = { [weak self] in
            guard let self, let engine, let id = currentReceiverID else { return nil }
            let mode = engine.options.soundOnly ? "audio" : Preferences.shared.screenMode(for: id).rawValue
            return (id, engine, mode)
        }
        together.hooks.stopMirror = { [weak self] in self?.stopMirror() }
        together.hooks.options = {
            var o = MirrorOptions()
            o.bitrate = Preferences.shared.bitrate
            o.audio = Preferences.shared.audio
            o.display = Preferences.shared.display
            o.codec = CodecChoice(rawValue: Preferences.shared.codec) ?? .auto
            o.sizeMode = SizeMode(rawValue: Preferences.shared.sizeMode) ?? .fit
            o.preset = QualityPreset.parse(Preferences.shared.preset) ?? .balanced
            o.warmth = Preferences.shared.warmth
            return o
        }
        together.hooks.restart = { [weak self] _ in self?.restartIfStreaming() }
        together.hooks.changed = { [weak self] in self?.rebuildMenu() }
    }
}
