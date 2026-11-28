// Showing on several TVs, for people who know nothing about it: three ready choices instead of roles and modes, the
// TVs ticked, and a playing group in plain words. The same choices and words as GNOME's
// (gnome-extension/luch@toxblh.github.io/group.js), tested alike; the dialog and the menu are LuchApp/Together.swift.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchProto

public enum GroupPreset: String, CaseIterable {
    /// This screen and its sound on every TV, all at one delay (rooms, a wall of TVs).
    case screen
    /// This Mac's sound on every TV, as speakers in every room.
    case music
    /// The picture on one TV right away, its sound on the others, waiting for the picture.
    case split

    public var title: String {
        switch self {
        case .screen: return "This Screen on All TVs"
        case .music: return "Music in Every Room"
        case .split: return "Screen Here, Sound There"
        }
    }

    public var detail: String {
        switch self {
        case .screen: return "Picture and sound everywhere, all in step"
        case .music: return "Only this Mac's sound, the TVs as speakers"
        case .split: return "The picture on one TV at once, its sound on the others, matched to it"
        }
    }

    /// The timing this choice plays with unless the person picked another under More Options.
    public var mode: SyncMode { self == .split ? .interactive : .watching }
}

/// What the Play on Several TVs dialog asks for: the TVs in the order of the list, the choice, the picture's TV for
/// `split`, and the timing under More Options. Every TV is ticked to begin with: the usual wish is "everywhere".
public struct GroupChoice {
    public let receivers: [(id: String, name: String)]
    public var preset = GroupPreset.screen
    public private(set) var ticked: Set<String>
    public private(set) var picture: String?
    /// The timing picked under More Options, nil for the choice's own. Music always plays in step.
    public var timing: SyncMode?

    public init(_ receivers: [(id: String, name: String)]) {
        self.receivers = receivers
        ticked = Set(receivers.map(\.id))
        picture = receivers.first?.id
    }

    public mutating func tick(_ id: String, _ on: Bool) {
        if on { ticked.insert(id) } else { ticked.remove(id) }
        // The picture's TV unticked: the first one still ticked takes it.
        if let p = picture, !ticked.contains(p) {
            picture = receivers.map(\.id).first(where: ticked.contains) ?? p
        }
    }

    public mutating func setPicture(_ id: String) {
        picture = id
        ticked.insert(id)
    }

    /// The members in order with their roles, the timing and whether it is the Mac's sound only; or why not yet.
    public func plan() -> Result<(members: [(id: String, role: GroupRole)], mode: SyncMode, soundOnly: Bool), Refusal> {
        let ids = receivers.map(\.id).filter(ticked.contains)
        guard ids.count >= 2 else { return .failure(Refusal(description: "Tick two TVs or more")) }
        switch preset {
        case .music:
            return .success((ids.map { ($0, .audio) }, .watching, true))
        case .screen:
            return .success((ids.map { ($0, .both) }, timing ?? .watching, false))
        case .split:
            // The picture's TV leads (the first one shows it) and plays no sound; the others are its speakers.
            let lead = picture.flatMap { ids.contains($0) ? $0 : nil } ?? ids[0]
            let rest = ids.filter { $0 != lead }.map { ($0, GroupRole.audio) }
            return .success(([(lead, .video)] + rest, timing ?? .interactive, false))
        }
    }

    public struct Refusal: Error, CustomStringConvertible {
        public let description: String
    }
}

/// A playing group in plain words, from its stats (`d_video_ms`, `d_audio_ms`, `mode`, `members`).
public enum GroupWords {
    /// How far behind this screen the TVs play.
    public static func delay(_ g: [String: Any]) -> String {
        let dv = (g["d_video_ms"] as? NSNumber)?.doubleValue ?? 0
        let da = (g["d_audio_ms"] as? NSNumber)?.doubleValue ?? 0
        if g["mode"] as? String != "interactive" {
            if da <= 0 { return "Matching the TVs to each other…" }
            return "All in step, \((da / 10).rounded() / 100) s behind this screen"
        }
        if da - dv < 5 { return "Picture and sound right away" }
        return "Picture right away, sound waits \(Int(((da - dv) / 10).rounded()) * 10) ms for it"
    }

    /// What a member plays, in words.
    public static func role(_ raw: String) -> String {
        switch raw {
        case "video": return "picture"
        case "audio": return "sound"
        default: return "picture and sound"
        }
    }

    /// "Together: Living room, Kitchen".
    public static func title(_ names: [String]) -> String { "Together: " + names.joined(separator: ", ") }
}
