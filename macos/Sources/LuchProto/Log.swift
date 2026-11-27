// Tiny logging helper: everything goes to stderr so stdout stays parseable, and the app's lines also to a bounded
// file a problem report takes (Report.swift in LuchReport).
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

private let logStart = Date()

/// When set, every line is also appended here (a bundled .app launched with `open` has no terminal).
public var logFile: String? {
    didSet {
        sinkLock.lock()
        sink = logFile.map { RotatingLog(path: $0) }
        sinkLock.unlock()
    }
}

private let sinkLock = NSLock()
private var sink: RotatingLog?

/// The app's log: `~/Library/Logs/Luch/luch.log`, where Console.app lists it under Log Reports.
public func defaultLogFile() -> String {
    let dir = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/Luch", isDirectory: true)
    try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir.appendingPathComponent("luch.log").path
}

public func log(_ message: String) {
    let t = Date().timeIntervalSince(logStart)
    let line = String(format: "[%7.3f] %@\n", t, message)
    FileHandle.standardError.write(Data(line.utf8))
    sinkLock.lock()
    let file = sink
    sinkLock.unlock()
    // The file outlives a launch: its lines carry the wall clock too.
    file?.append(RotatingLog.stamp(Date()) + " " + line)
}

public func errnoString() -> String {
    String(cString: strerror(errno))
}

/// A log file kept to two of `limit` bytes: when the current one is full it becomes `<path>.1` (the older `.1` goes)
/// and a new one starts. A session's stats line a second, so a few hours of sessions fit.
public final class RotatingLog {
    public let path: String
    public let limit: Int
    private let lock = NSLock()
    private var handle: FileHandle?
    private var size = 0

    public init(path: String, limit: Int = 1 << 20) {
        self.path = path
        self.limit = limit
    }

    deinit { try? handle?.close() }

    public var older: String { path + ".1" }

    public func append(_ line: String) {
        let data = Data(line.utf8)
        lock.lock()
        defer { lock.unlock() }
        if handle == nil { open() }
        if size > 0 && size + data.count > limit {
            try? handle?.close()
            handle = nil
            let fm = FileManager.default
            try? fm.removeItem(atPath: older)
            try? fm.moveItem(atPath: path, toPath: older)
            open()
        }
        guard let handle else { return }
        handle.write(data)
        size += data.count
    }

    private func open() {
        let fm = FileManager.default
        if !fm.fileExists(atPath: path) { fm.createFile(atPath: path, contents: nil) }
        handle = FileHandle(forWritingAtPath: path)
        size = Int(handle?.seekToEndOfFile() ?? 0)
    }

    /// The newest `maxBytes` of the log (the older file first), cut at a line start.
    public static func tail(_ path: String, maxBytes: Int) -> String {
        var data = Data()
        for p in [path + ".1", path] {
            if let d = FileManager.default.contents(atPath: p) { data.append(d) }
        }
        guard data.count > maxBytes else { return String(decoding: data, as: UTF8.self) }
        var cut = data.suffix(maxBytes)
        if let nl = cut.firstIndex(of: 0x0a) { cut = cut[cut.index(after: nl)...] }
        return String(decoding: cut, as: UTF8.self)
    }

    private static let stamper: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd HH:mm:ss.SSS"
        return f
    }()

    static func stamp(_ d: Date) -> String { stamper.string(from: d) }
}
