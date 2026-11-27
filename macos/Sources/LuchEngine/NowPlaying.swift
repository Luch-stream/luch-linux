// What this Mac plays, for the receiver's `now_playing` (PROTOCOL.md 4d). Since macOS 15.4 MediaRemote, the only
// source of it, answers Apple's own processes and returns nothing to others. The system perl is one of Apple's, so
// the small LuchNowPlaying library is loaded into it and writes JSON lines that we read here (the way the open
// mediaremote-adapter does it).
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchProto

public struct NowPlayingInfo: Equatable {
    public var present = false
    public var playing = false
    public var title: String?
    public var artist: String?
    public var album: String?
    public var app: String?
    public var dur: Double?
    public var elapsed: Double?
    public var rate: Double?
    /// When `elapsed` was true, in seconds since 1970.
    public var at: Double?
    /// A file with the cover; a new cover comes under a new name.
    public var art: String?
    public var artMime: String?

    public init() {}

    /// One line of the helper; nil for anything else.
    public init?(line: String) {
        guard let o = (try? JSONSerialization.jsonObject(with: Data(line.utf8))) as? [String: Any] else { return nil }
        func text(_ k: String) -> String? { (o[k] as? String).flatMap { $0.isEmpty ? nil : $0 } }
        func number(_ k: String) -> Double? { (o[k] as? NSNumber)?.doubleValue }
        present = (o["present"] as? NSNumber)?.boolValue ?? false
        playing = (o["playing"] as? NSNumber)?.boolValue ?? false
        title = text("title")
        artist = text("artist")
        album = text("album")
        app = text("app")
        dur = number("dur")
        elapsed = number("elapsed")
        rate = number("rate")
        at = number("at")
        art = text("art")
        artMime = text("art_mime")
    }

    /// The position now: the last one known, moved on by the time since, at the rate it plays.
    public func position(now: Date) -> Double? {
        guard var pos = elapsed else { return nil }
        if playing, let at { pos += max(0, now.timeIntervalSince1970 - at) * (rate ?? 1) }
        if let dur, dur > 0 { pos = min(pos, dur) }
        return (pos * 10).rounded() / 10
    }

    /// The protocol message; `art` is the cover's link on this Mac, when there is one.
    public func message(now: Date = Date(), art link: String? = nil) -> [String: Any] {
        var m: [String: Any] = ["t": "now_playing", "state": !present ? "stopped" : playing ? "playing" : "paused"]
        guard present else { return m }
        if let title { m["title"] = title }
        if let artist { m["artist"] = artist }
        if let album { m["album"] = album }
        if let app { m["app"] = app }
        if let dur, dur > 0 { m["dur"] = (dur * 10).rounded() / 10 }
        if let pos = position(now: now) { m["pos"] = pos }
        if let link { m["art"] = link }
        // What the media keys reach (MediaKeys): no seeking to a point.
        m["can"] = ["play_pause", "next", "previous"]
        return m
    }
}

/// Runs the helper for as long as it is needed and reports each change.
public final class NowPlayingWatcher {
    public var onChange: ((NowPlayingInfo) -> Void)?
    private var process: Process?
    private var input: Pipe?
    private var carry = Data()
    private let lock = NSLock()

    /// Perl loads the library and calls it; it returns only when our end of its stdin closes.
    static let script = """
    use DynaLoader;
    my $l = DynaLoader::dl_load_file($ARGV[0], 0) or die DynaLoader::dl_error();
    my $s = DynaLoader::dl_find_symbol($l, "luch_now_playing") or die DynaLoader::dl_error();
    DynaLoader::dl_install_xsub("main::watch", $s);
    watch();
    """

    public init() {}

    deinit { stop() }

    /// The library next to the executable (a build) or in the app's Frameworks; `LUCH_NOW_PLAYING_LIB` overrides.
    public static func library() -> String? {
        let fm = FileManager.default
        if let path = ProcessInfo.processInfo.environment["LUCH_NOW_PLAYING_LIB"], fm.fileExists(atPath: path) {
            return path
        }
        let exe = Bundle.main.executableURL?.resolvingSymlinksInPath().deletingLastPathComponent()
        let dirs = [exe, exe?.appendingPathComponent("../Frameworks"), Bundle.main.privateFrameworksURL]
        for dir in dirs.compactMap({ $0 }) {
            let path = dir.appendingPathComponent("libLuchNowPlaying.dylib").standardizedFileURL.path
            if fm.fileExists(atPath: path) { return path }
        }
        return nil
    }

    @discardableResult
    public func start(library: String? = NowPlayingWatcher.library()) -> Bool {
        guard let library else {
            log("now playing: libLuchNowPlaying.dylib not found, the receiver will not see what plays")
            return false
        }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/perl")
        p.arguments = ["-e", Self.script, library]
        let input = Pipe(), output = Pipe()
        p.standardInput = input
        p.standardOutput = output
        p.standardError = FileHandle.standardError
        output.fileHandleForReading.readabilityHandler = { [weak self] h in
            let data = h.availableData
            if data.isEmpty {
                h.readabilityHandler = nil
                return
            }
            self?.take(data)
        }
        do {
            try p.run()
        } catch {
            log("now playing: cannot run perl: \(error.localizedDescription)")
            return false
        }
        lock.lock()
        process = p
        self.input = input
        lock.unlock()
        return true
    }

    private func take(_ data: Data) {
        carry.append(data)
        while let nl = carry.firstIndex(of: 10) {
            let line = String(decoding: carry[carry.startIndex..<nl], as: UTF8.self)
            carry.removeSubrange(carry.startIndex...nl)
            if let info = NowPlayingInfo(line: line) { onChange?(info) }
        }
    }

    public func stop() {
        lock.lock()
        let p = process, input = self.input
        process = nil
        self.input = nil
        lock.unlock()
        try? input?.fileHandleForWriting.close()
        if let p, p.isRunning {
            DispatchQueue.global().asyncAfter(deadline: .now() + 1) { if p.isRunning { p.terminate() } }
        }
    }
}
