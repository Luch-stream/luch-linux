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
import ServiceManagement

final class Preferences {
    static let shared = Preferences()
    private let defaults = UserDefaults.standard

    /// What ultra-low latency puts first, per receiver id; nil until asked.
    func ultra(for id: String) -> UltraPriority? {
        (defaults.string(forKey: "ultra." + id)).flatMap(UltraPriority.init(rawValue:))
    }

    func setUltra(_ p: UltraPriority, for id: String) {
        defaults.set(p.rawValue, forKey: "ultra." + id)
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
    private var engine: Engine?
    private var state = "Idle"
    private var statsLine = "Not streaming"
    private var statsItem: NSMenuItem?
    private var timer: Timer?
    private var receivers: [Receiver] = []
    private var pendingMenuRefresh = false
    private var menuOpen = false
    private var server: LocalServer?
    private var currentReceiverID: String?
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
        logFile = Bundle.main.bundleURL.deletingLastPathComponent()
            .appendingPathComponent("luch-app.log").path
        log("app: launched, bundle \(Bundle.main.bundleURL.lastPathComponent)")

        NSApp.setActivationPolicy(.accessory)
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        statusItem.button?.image = NSImage(systemSymbolName: "dot.radiowaves.left.and.right",
                                           accessibilityDescription: "Luch")
        statusItem.button?.toolTip = "Luch"
        statusItem.menu = menu
        menu.delegate = self

        discovery.onUpdate = { [weak self] in
            DispatchQueue.main.async { self?.refreshReceivers() }
        }
        discovery.start()
        let identity = senderIdentity()
        discovery.publish(name: identity.name, id: identity.id)
        startLocalServer()
        caster.onChange = { [weak self] in DispatchQueue.main.async { self?.rebuildMenu() } }

        rebuildMenu()
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in self?.tick() }

        if Preferences.shared.mirrorOnLaunch {
            DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in self?.mirrorIfSingle() }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        engine?.stop(reason: "quit")
    }

    // MARK: - local API (luchctl)

    let caster = Caster()

    private func startLocalServer() {
        let server = LocalServer()
        server.handler = { [weak self] request in
            guard let self else { return ["error": "app is shutting down"] }
            // What waits for a receiver's answer runs here, on the socket's thread, not on the main one.
            if Caster.commands.contains(request["cmd"] as? String ?? "") {
                var known: [Receiver] = []
                var mirroring: String?
                DispatchQueue.main.sync {
                    known = self.receivers
                    mirroring = self.currentReceiverID
                }
                return self.caster.handle(request, receivers: known, mirroring: mirroring)
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
            return ["receivers": receivers.map { receiver in
                ["id": receiver.id, "name": receiver.name, "model": receiver.model,
                 "address": receiver.address, "iface": receiver.interfaceName,
                 "port": Int(receiver.port),
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
            let ultra = (request["ultra"] as? String).flatMap(UltraPriority.init(rawValue:))
            if request["ultra"] != nil, ultra == nil { return ["error": "ultra: speed, quality or mac"] }
            startMirror(receiver, extend: request["extend"] as? Bool ?? false, ultra: ultra)
            return ["ok": true, "receiver": receiver.name]
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

    private func refreshReceivers() {
        receivers = discovery.all()
        rebuildMenu()
    }

    private func rebuildMenu() {
        // Rebuilt under the pointer, an open menu would close its submenus: wait for it to close.
        if menuOpen {
            pendingMenuRefresh = true
            return
        }
        menu.removeAllItems()

        if receivers.isEmpty {
            let item = NSMenuItem(title: "No receivers found", action: nil, keyEquivalent: "")
            item.isEnabled = false
            menu.addItem(item)
        }
        for (index, receiver) in receivers.enumerated() {
            let item = NSMenuItem(title: receiver.name, action: nil, keyEquivalent: "")
            item.submenu = receiverMenu(receiver)
            if caster.existing(receiver.id)?.showing != nil || currentReceiverID == receiver.id { item.state = .on }
            if index == 0 { item.image = NSImage(systemSymbolName: "tv", accessibilityDescription: nil) }
            menu.addItem(item)
        }

        menu.addItem(.separator())

        let stats = NSMenuItem(title: statsLine, action: nil, keyEquivalent: "")
        stats.isEnabled = false
        statsItem = stats
        menu.addItem(stats)

        let stateItem = NSMenuItem(title: state, action: nil, keyEquivalent: "")
        stateItem.isEnabled = false
        menu.addItem(stateItem)

        menu.addItem(.separator())

        let audio = NSMenuItem(title: "Audio", action: #selector(toggleAudio), keyEquivalent: "")
        audio.target = self
        audio.state = Preferences.shared.audio ? .on : .off
        menu.addItem(audio)

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
        // Tucked into a submenu: the top level stays short, and the row gets the full width it needs.
        let warmParent = NSMenuItem(title: "Warm picture", action: nil, keyEquivalent: "")
        let warmMenu = NSMenu()
        warmMenu.addItem(warmItem)
        warmParent.submenu = warmMenu
        menu.addItem(warmParent)

        let quality = NSMenuItem(title: "Quality", action: nil, keyEquivalent: "")
        let qualityMenu = NSMenu()
        for preset in QualityPreset.allCases {
            // Warn about the cost, do not silently change the user's choice.
            let onBattery = engine?.deviceOnBattery ?? false
            let hint = onBattery && preset != .low ? " — drains the battery faster" : ""
            _ = preset
            let item = NSMenuItem(title: preset.label + hint, action: #selector(setPreset(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = preset.rawValue
            item.state = Preferences.shared.preset == preset.rawValue ? .on : .off
            qualityMenu.addItem(item)
        }
        quality.submenu = qualityMenu
        menu.addItem(quality)

        let size = NSMenuItem(title: "Size", action: nil, keyEquivalent: "")
        let sizeMenu = NSMenu()
        for mode in SizeMode.allCases {
            let item = NSMenuItem(title: mode.rawValue, action: #selector(setSizeMode(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = mode.rawValue
            item.state = Preferences.shared.sizeMode == mode.rawValue ? .on : .off
            sizeMenu.addItem(item)
        }
        size.submenu = sizeMenu
        menu.addItem(size)

        let codec = NSMenuItem(title: "Codec", action: nil, keyEquivalent: "")
        let codecMenu = NSMenu()
        for choice in CodecChoice.allCases {
            let item = NSMenuItem(title: choice.rawValue.uppercased(), action: #selector(setCodec(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = choice.rawValue
            item.state = Preferences.shared.codec == choice.rawValue ? .on : .off
            codecMenu.addItem(item)
        }
        codec.submenu = codecMenu
        menu.addItem(codec)

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
        menu.addItem(bitrate)

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
        menu.addItem(display)

        menu.addItem(.separator())

        let launch = NSMenuItem(title: "Mirror on launch", action: #selector(toggleMirrorOnLaunch), keyEquivalent: "")
        launch.target = self
        launch.state = Preferences.shared.mirrorOnLaunch ? .on : .off
        menu.addItem(launch)

        let login = NSMenuItem(title: "Start at Login", action: #selector(toggleLogin), keyEquivalent: "")
        login.target = self
        login.state = SMAppService.mainApp.status == .enabled ? .on : .off
        menu.addItem(login)

        menu.addItem(.separator())
        let quit = NSMenuItem(title: "Quit Luch", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        menu.addItem(quit)
    }

    private func tick() {
        guard let stats = engine?.lastStats else {
            if engine == nil && statsLine != "Not streaming" {
                statsLine = "Not streaming"
                statsItem?.title = statsLine
            }
            return
        }
        var line = String(format: "≈ %.0f ms · host %.1f · rtt %.1f", stats.estMs, stats.hostMs, stats.rttMs)
        if let decode = stats.decodeMs { line += String(format: " · dec %.1f", decode) }
        if let battery = stats.battery {
            line += " · 📱\(battery)%"
            if stats.charging == true { line += "⚡" }
        }
        if let thermal = stats.thermal, thermal != "none" { line += " · \(thermal)" }
        if let headroom = stats.thermalHeadroom {
            // Warn, never throttle on its own: the user decides whether to drop the quality.
            line += headroom < 0.3 ? String(format: " · 🔥 %.2f", headroom)
                                   : String(format: " · 🌡%.2f", headroom)
        }
        line += String(format: " · %.0f fps", stats.fps)
        if line != statsLine {
            statsLine = line
            statsItem?.title = line
        }
    }

    // MARK: - actions

    @objc private func mirrorIfSingle() {
        guard engine == nil, receivers.count == 1 else { return }
        startMirror(receivers[0])
    }

    private func startMirror(_ receiver: Receiver, soundOnly: Bool = false, extend: Bool = false,
                             ultra: UltraPriority? = nil) {
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
                DispatchQueue.main.async { [weak self] in self?.beginMirror(receiver, soundOnly: soundOnly, extend: extend, ultra: ultra) }
            }
            return
        }
        beginMirror(receiver, soundOnly: soundOnly, extend: extend, ultra: ultra)
    }

    private func beginMirror(_ receiver: Receiver, soundOnly: Bool, extend: Bool, ultra: UltraPriority?) {
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
                state = "Allow Luch in Screen Recording, then reopen Luch"
                rebuildMenu()
                if let pane = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") {
                    NSWorkspace.shared.open(pane)
                }
                return
            }
        }

        stopMirror()
        var options = MirrorOptions()
        options.bitrate = Preferences.shared.bitrate
        options.audio = Preferences.shared.audio
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
        let engine = Engine(options: options)
        engine.shareArt = { [caster] file, client, me in caster.shareArt(file, client: client, me: me) }
        engine.onWaitingForAllow = { [weak self] in
            DispatchQueue.main.async {
                self?.state = "Press Allow on \(receiver.name) (left, then OK on its remote)"
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
                self.state = "Idle (\(reason))"
                self.statsLine = "Not streaming"
                self.statsItem?.title = self.statsLine
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
                let codec = engine.plan?.codec.label ?? ""
                let batteryNote = engine.deviceOnBattery ? " · 🔋 on battery" : ""
                DispatchQueue.main.async {
                    self?.state = soundOnly ? "Playing sound on \(receiver.name)"
                        : extend ? "Extended to \(receiver.name) · \(codec)"
                        : ultra != nil ? "Ultra-low latency to \(receiver.name) · \(engine.ultraMode.map { "\($0)" } ?? "") · \(codec)"
                        : "Mirroring to \(receiver.name) · \(codec)\(batteryNote)"
                    self?.rebuildMenu()
                }
            } catch {
                DispatchQueue.main.async {
                    self?.state = "Error: \(error.localizedDescription)"
                    self?.engine = nil
                    self?.rebuildMenu()
                }
            }
        }
    }

    private func apply(_ stats: SessionStats) {
        var line = String(format: "≈ %.0f ms · host %.1f · rtt %.1f", stats.estMs, stats.hostMs, stats.rttMs)
        if let decode = stats.decodeMs { line += String(format: " · dec %.1f", decode) }
        if let battery = stats.battery {
            line += " · 📱\(battery)%"
            if stats.charging == true { line += "⚡" }
        }
        if let thermal = stats.thermal, thermal != "none" { line += " · \(thermal)" }
        if let headroom = stats.thermalHeadroom {
            // Warn, never throttle on its own: the user decides whether to drop the quality.
            line += headroom < 0.3 ? String(format: " · 🔥 %.2f", headroom)
                                   : String(format: " · 🌡%.2f", headroom)
        }
        line += String(format: " · %.0f fps", stats.fps)
        statsLine = line
        statsItem?.title = line
    }

    @objc private func stopMirror() {
        guard let engine else { return }
        self.engine = nil
        currentReceiverID = nil
        currentReceiver = nil
        state = "Stopping…"
        statsLine = "Not streaming"
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
        startMirror(receiver)
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
            state = "Login item: \(error.localizedDescription)"
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
        m.addItem(.run("Mirror Screen", symbol: "rectangle.on.rectangle") { [weak self] in self?.startMirror(r) })
        m.addItem(.run("Extend Screen", symbol: "rectangle.split.2x1") { [weak self] in
            self?.startMirror(r, extend: true)
        })
        m.addItem(.run("Mirror · Ultra-Low Latency", symbol: "bolt") { [weak self] in
            guard let self, let p = self.ultraPriority(for: r) else { return }
            self.startMirror(r, ultra: p)
        })
        let ultraItem = NSMenuItem(title: "Ultra-Low Latency Puts First", action: nil, keyEquivalent: "")
        let ultraMenu = NSMenu()
        let saved = Preferences.shared.ultra(for: r.id)
        for p in UltraPriority.allCases {
            let item = NSMenuItem.run(p.label) { Preferences.shared.setUltra(p, for: r.id) }
            if p == saved { item.state = .on }
            ultraMenu.addItem(item)
        }
        ultraItem.submenu = ultraMenu
        m.addItem(ultraItem)
        m.addItem(.run("Play Sound Only", symbol: "hifispeaker") { [weak self] in self?.startMirror(r, soundOnly: true) })
        if mirroringHere {
            m.addItem(.run("Stop Mirroring", symbol: "stop.circle") { [weak self] in self?.stopMirror() })
        }

        m.addItem(.separator())
        m.addItem(.run("Show File…", symbol: "doc") { [weak self] in self?.chooseFiles(for: r) })
        let link = Self.clipboardLink()
        let linkTitle = link.flatMap { URL(string: $0)?.host }.map { "Show Link from Clipboard (\($0))" }
            ?? "Show Link from Clipboard"
        m.addItem(.run(linkTitle, enabled: link != nil, symbol: "link") { [weak self] in
            if let link { self?.send(["cmd": "cast", "items": [link]], to: r) }
        })

        if let s = caster.existing(r.id), let kind = s.showing {
            m.addItem(.separator())
            addShowing(kind, session: s, to: m, receiver: r)
        }

        m.addItem(.separator())
        m.addItem(volumeItem(r))
        m.addItem(.run("Put to Sleep", symbol: "moon") { [weak self] in self?.send(["cmd": "sleep"], to: r) })
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
    private func send(_ request: [String: Any], to r: Receiver) {
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let self else { return }
            let response = self.caster.run(request, on: r)
            DispatchQueue.main.async {
                if let error = response["error"] as? String {
                    let alert = NSAlert()
                    alert.messageText = r.name
                    alert.informativeText = error
                    NSApp.activate(ignoringOtherApps: true)
                    alert.runModal()
                }
                self.rebuildMenu()
            }
        }
    }

    private func chooseFiles(for r: Receiver) {
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
        if pendingMenuRefresh {
            pendingMenuRefresh = false
            rebuildMenu()
        }
    }
}

// MARK: - ultra-low latency: asked once per TV what it should put first

extension AppDelegate {
    /// What this TV's ultra-low latency mode puts first: asked the first time, then kept (and changed in the menu).
    fileprivate func ultraPriority(for r: Receiver) -> UltraPriority? {
        if let p = Preferences.shared.ultra(for: r.id) { return p }
        let alert = NSAlert()
        alert.messageText = "Ultra-low latency on \(r.name)"
        alert.informativeText = "This Mac will draw its desktop at one of the TV's own modes, so no frame is scaled on "
            + "either side; its own screen shows a copy. What should it put first? It is asked once for this TV and "
            + "can be changed in its menu."
        for p in [UltraPriority.speed, .quality, .likeMac] {
            alert.addButton(withTitle: p.label)
        }
        NSApp.activate(ignoringOtherApps: true)
        let picked: UltraPriority
        switch alert.runModal() {
        case .alertFirstButtonReturn: picked = .speed
        case .alertSecondButtonReturn: picked = .quality
        default: picked = .likeMac
        }
        Preferences.shared.setUltra(picked, for: r.id)
        return picked
    }
}
