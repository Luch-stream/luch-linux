// The sound of a session to one receiver (PROTOCOL.md 4, "Audio modes"), and what this Mac keeps per receiver for
// playing in step: the sound mode, each output route's correction and the route last reported. As luchd's
// `audio`, `sync-offsets` and `sync-routes` choices, kept here in the app's defaults.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchProto

public enum AudioMode: String, CaseIterable {
    /// The picture held back to match where the receiver cannot make its sound short (Android 5 and 6), else as it
    /// comes.
    case auto
    /// As it comes, `audio_latency_ms` after the picture.
    case on
    /// Picture only, the lowest latency.
    case off
    /// Always held to match.
    case sync

    public var label: String {
        switch self {
        case .auto: return "Auto"
        case .on: return "On"
        case .off: return "Off (lowest latency)"
        case .sync: return "In Sync with the Picture"
        }
    }

    /// The `audio` part of `start`, or nil to send the picture only.
    public func startBlock(limits: [String], frameMs: Int) -> [String: Any]? {
        let sync: Bool
        switch self {
        case .off: return nil
        case .sync: sync = true
        case .on: sync = false
        case .auto: sync = limits.contains("audio_lag")
        }
        var a: [String: Any] = ["codec": "audio/opus", "rate": 48_000, "channels": 2, "frame_ms": frameMs]
        if sync { a["sync"] = true }
        return a
    }
}

public final class SyncStore {
    public static let shared = SyncStore()
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// The sound mode chosen for a receiver ("auto" when none).
    public func audioMode(_ id: String) -> AudioMode {
        defaults.string(forKey: "audio." + id).flatMap(AudioMode.init(rawValue:)) ?? .auto
    }

    public func setAudioMode(_ mode: AudioMode, for id: String) {
        defaults.set(mode.rawValue, forKey: "audio." + id)
    }

    /// A receiver's correction for one output route (Sync.offsetKey), nil when none was set.
    public func offset(_ key: String) -> Double? {
        (defaults.object(forKey: "syncOffset." + key) as? NSNumber)?.doubleValue
    }

    public func setOffset(_ ms: Double, key: String) {
        defaults.set(ms, forKey: "syncOffset." + key)
    }

    /// The output route a receiver last reported ("unknown" before it ever did).
    public func route(_ id: String) -> String {
        defaults.string(forKey: "syncRoute." + id) ?? "unknown"
    }

    public func rememberRoute(_ route: String, for id: String) {
        defaults.set(route, forKey: "syncRoute." + id)
    }

    /// The correction for the route the receiver uses now: what a group starts it with.
    public func currentOffset(_ id: String) -> Double {
        offset(Sync.offsetKey(id, route: route(id))) ?? 0
    }
}
