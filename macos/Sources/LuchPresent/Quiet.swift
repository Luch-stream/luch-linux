// Do not disturb while presenting or mirroring: notification banners hidden, and back as they were afterwards; the
// rules of luchd's quiet.rs.
//
// The setting is "ask" (the default), "on" or "off". On "ask" the app asks once; the answer holds until it quits.
// What was changed is written down first, so a crash mid-session is undone at the next start.
//
// macOS has no API for Focus. What works on 14 and later without private frameworks is Shortcuts' own "Set Focus"
// action, run with `shortcuts run`: two shortcuts, "Luch Quiet On" and "Luch Quiet Off", which the app makes and
// hands to Shortcuts to add (one click each). Whether Focus is already on is read from the Focus database when macOS
// lets us; a Focus the person turned on is then left alone. Mirroring needs less of this: macOS itself hides
// notifications while the screen is captured, unless "Allow notifications when mirroring or sharing the display"
// is on.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchProto

/// Where the banners switch lives; Focus through Shortcuts, or a stand-in in tests.
public protocol Banners: AnyObject {
    /// Whether banners are shown now; nil when there is no way to change it (the shortcuts are not there).
    func shown() -> Bool?
    func set(_ on: Bool)
}

public enum QuietMode: String, CaseIterable {
    case ask, on, off

    public static func parse(_ s: String) -> QuietMode? {
        QuietMode(rawValue: s.trimmingCharacters(in: .whitespacesAndNewlines))
    }
}

public final class Quiet {
    private let banners: Banners
    /// The file that says banners were on before we turned them off; nil: nothing is written (tests).
    private let note: URL?
    public private(set) var mode: QuietMode
    private var answer: Bool?
    private var sessions = 0
    /// Banners were shown and we turned them off.
    private var hid = false
    private let lock = NSRecursiveLock()

    public init(banners: Banners, note: URL?, mode: QuietMode) {
        self.banners = banners
        self.note = note
        self.mode = mode
    }

    deinit {
        sessions = 0
        apply()
    }

    public func setMode(_ m: QuietMode) {
        lock.lock()
        defer { lock.unlock() }
        mode = m
        apply()
    }

    /// A presentation or a mirroring session began; true when the person should be asked now.
    public func start() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        sessions += 1
        apply()
        return mode == .ask && answer == nil
    }

    public func end() {
        lock.lock()
        defer { lock.unlock() }
        sessions = max(0, sessions - 1)
        apply()
    }

    /// The app quits: banners back as they were, whatever still runs.
    public func endAll() {
        lock.lock()
        defer { lock.unlock() }
        sessions = 0
        apply()
    }

    /// The person's answer to the question: hide notifications (true) or not; kept until the app quits.
    public func answer(_ hide: Bool) {
        lock.lock()
        defer { lock.unlock() }
        answer = hide
        apply()
    }

    private var wanted: Bool {
        guard sessions > 0 else { return false }
        switch mode {
        case .on: return true
        case .off: return false
        case .ask: return answer == true
        }
    }

    /// Banners off while wanted, back on (only if we turned them off) when not.
    private func apply() {
        if wanted && !hid {
            if banners.shown() == true {
                // Written before the change: a crash right after it is still undone.
                if let note { try? Data("true".utf8).write(to: note) }
                banners.set(false)
                hid = true
            }
        } else if !wanted && hid {
            banners.set(true)
            hid = false
            if let note { try? FileManager.default.removeItem(at: note) }
        }
    }

    /// At start: banners back on if a crash left them off.
    public func recover() {
        guard let note else { return }
        if (try? String(contentsOf: note, encoding: .utf8))?.trimmingCharacters(in: .whitespacesAndNewlines) == "true" {
            banners.set(true)
        }
        try? FileManager.default.removeItem(at: note)
    }

    /// Counts a session in until the returned hold is let go, however the session ends; `ask` is called (on any
    /// thread) when the person should be asked.
    public func hold(ask: (() -> Void)? = nil) -> AnyObject {
        let h = Hold(self)
        if start() { ask?() }
        return h
    }

    private final class Hold {
        let quiet: Quiet
        init(_ q: Quiet) { quiet = q }
        deinit { quiet.end() }
    }

    // MARK: - the app's one Quiet

    static var dir: URL {
        let d = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Luch", isDirectory: true)
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    /// Made on first use from the saved setting, a crash's leftovers undone.
    public static let shared: Quiet = {
        let mode = (try? String(contentsOf: dir.appendingPathComponent("quiet"), encoding: .utf8))
            .flatMap(QuietMode.parse) ?? .ask
        let q = Quiet(banners: FocusShortcuts(), note: dir.appendingPathComponent("restore-banners"), mode: mode)
        q.recover()
        return q
    }()

    public static func save(_ m: QuietMode) {
        try? Data(m.rawValue.utf8).write(to: dir.appendingPathComponent("quiet"))
        shared.setMode(m)
    }
}

/// Focus (Do Not Disturb) through two shortcuts the app makes: Shortcuts' "Set Focus" is the only way to switch it
/// that macOS offers to an app (Hush, the open-source screen-share silencer, does the same).
public final class FocusShortcuts: Banners {
    public static let onName = "Luch Quiet On"
    public static let offName = "Luch Quiet Off"
    static let tool = "/usr/bin/shortcuts"

    public init() {}

    static func run(_ args: [String]) -> String? {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: tool)
        p.arguments = args
        let out = Pipe()
        p.standardOutput = out
        p.standardError = FileHandle.nullDevice
        p.standardInput = FileHandle.nullDevice
        guard (try? p.run()) != nil else { return nil }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        return p.terminationStatus == 0 ? String(decoding: data, as: UTF8.self) : nil
    }

    /// Both shortcuts are in the person's Shortcuts.
    public static var ready: Bool {
        let names = Set((run(["list"]) ?? "").split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) })
        return names.contains(onName) && names.contains(offName)
    }

    public func shown() -> Bool? {
        guard FocusShortcuts.ready else { return nil }
        return !(FocusShortcuts.focusOn() ?? false)
    }

    public func set(_ on: Bool) {
        let name = on ? FocusShortcuts.offName : FocusShortcuts.onName
        if FocusShortcuts.run(["run", name]) == nil { log("quiet: shortcut \"\(name)\" failed") }
    }

    /// Whether a Focus is on now, from the Focus database (`Assertions.json`); nil when macOS keeps it from us.
    static func focusOn(_ file: URL = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/DoNotDisturb/DB/Assertions.json")) -> Bool? {
        guard let d = try? Data(contentsOf: file) else { return nil }
        return focusOn(json: d)
    }

    static func focusOn(json: Data) -> Bool? {
        guard let v = (try? JSONSerialization.jsonObject(with: json)) as? [String: Any],
              let data = v["data"] as? [[String: Any]] else { return nil }
        return data.contains { !(($0["storeAssertionRecords"] as? [Any]) ?? []).isEmpty }
    }

    /// The two shortcuts as signed files Shortcuts can add: "Set Focus: Do Not Disturb on" and "off".
    public static func makeFiles(in dir: URL) throws -> [URL] {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return try [(onName, true), (offName, false)].map { name, on in
            let plain = dir.appendingPathComponent("\(name)-unsigned.shortcut")
            let signed = dir.appendingPathComponent("\(name).shortcut")
            try workflow(on: on).write(to: plain)
            try? FileManager.default.removeItem(at: signed)
            guard run(["sign", "--mode", "anyone", "--input", plain.path, "--output", signed.path]) != nil,
                  FileManager.default.fileExists(atPath: signed.path) else {
                throw PresentError.bad("Shortcuts could not sign \(name)")
            }
            try? FileManager.default.removeItem(at: plain)
            return signed
        }
    }

    /// A shortcut of one action, Set Focus (Do Not Disturb) on or off, as Shortcuts stores it.
    static func workflow(on: Bool) throws -> Data {
        let action: [String: Any] = [
            "WFWorkflowActionIdentifier": "is.workflow.actions.dnd.set",
            "WFWorkflowActionParameters": [
                "Enabled": on ? 1 : 0,
                "FocusModes": ["Identifier": "com.apple.donotdisturb.mode.default", "DisplayString": "Do Not Disturb"],
            ] as [String: Any],
        ]
        let w: [String: Any] = [
            "WFWorkflowClientVersion": "2607.0.2",
            "WFWorkflowMinimumClientVersion": 900,
            "WFWorkflowIcon": ["WFWorkflowIconStartColor": on ? 463_140_863 : 4_282_601_983,
                               "WFWorkflowIconGlyphNumber": 59511],
            "WFWorkflowImportQuestions": [Any](),
            "WFWorkflowTypes": [Any](),
            "WFWorkflowInputContentItemClasses": [Any](),
            "WFWorkflowActions": [action],
        ]
        return try PropertyListSerialization.data(fromPropertyList: w, format: .binary, options: 0)
    }
}
