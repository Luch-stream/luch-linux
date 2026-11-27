// The TV remote's media keys for the player on this Mac (PROTOCOL.md 4d): posted as the keyboard's own media keys,
// so whatever answers them (Music, Spotify, a browser's video) obeys. Without Accessibility access macOS drops them
// silently: the first key asks for it. The Linux sender does the same through MPRIS.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit

public enum MediaKeys {
    private static var asked = false
    /// NX_KEYTYPE_* of IOKit's ev_keymap.h.
    public static func code(_ key: String) -> Int32? {
        switch key {
        case "play_pause", "play", "pause": return 16  // NX_KEYTYPE_PLAY
        case "next": return 17                         // NX_KEYTYPE_NEXT
        case "previous": return 18                     // NX_KEYTYPE_PREVIOUS
        case "seek_forward": return 19                 // NX_KEYTYPE_FAST
        case "seek_back": return 20                    // NX_KEYTYPE_REWIND
        default: return nil
        }
    }

    /// Presses and lets go of the media key for `key`; false when there is none for it.
    @discardableResult
    public static func post(_ key: String) -> Bool {
        guard let code = code(key) else { return false }
        if !asked, !InputPoster.allowed {
            asked = true
            InputPoster.requestAccess()
        }
        return postSystem(code, down: true) && postSystem(code, down: false)
    }

    /// One press or release of the system key `code` (NX_KEYTYPE_*): media, volume, brightness. A keyboard on the
    /// receiver sends these too (PROTOCOL.md 4i).
    @discardableResult
    public static func postSystem(_ code: Int32, down: Bool) -> Bool {
        let flags = NSEvent.ModifierFlags(rawValue: down ? 0xa00 : 0xb00)
        let data1 = Int((code << 16) | ((down ? 0xa : 0xb) << 8))
        guard let e = NSEvent.otherEvent(with: .systemDefined, location: .zero, modifierFlags: flags, timestamp: 0,
                                         windowNumber: 0, context: nil, subtype: 8, data1: data1, data2: -1) else {
            return false
        }
        e.cgEvent?.post(tap: .cghidEventTap)
        return true
    }
}
