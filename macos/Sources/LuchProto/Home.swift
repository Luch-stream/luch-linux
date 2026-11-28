// Where the app keeps its state. LUCH_HOME (tests only) moves all of it, the control socket included, so a second
// instance can run beside the installed app without sharing its identity, pairings or socket; such an instance also
// keeps to itself: it does not announce itself, look for receivers on its own, nor take the web receiver's port.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum Home {
    /// LUCH_HOME, when set (a test instance).
    public static var override: String? {
        let v = ProcessInfo.processInfo.environment["LUCH_HOME"] ?? ""
        return v.isEmpty ? nil : v
    }

    /// True in a test instance started with LUCH_HOME.
    public static var isolated: Bool { override != nil }

    /// `~/Library/Application Support/Luch`, or LUCH_HOME; made when missing.
    public static var support: URL {
        let dir = override.map { URL(fileURLWithPath: $0, isDirectory: true) }
            ?? FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent("Library/Application Support/Luch", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// `~/Library/Logs/Luch`, or `logs` under LUCH_HOME.
    public static var logs: URL {
        override.map { URL(fileURLWithPath: $0, isDirectory: true).appendingPathComponent("logs", isDirectory: true) }
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/Luch", isDirectory: true)
    }
}
