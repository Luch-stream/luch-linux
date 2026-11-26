// The TV remote's media keys for the player on this Mac (PROTOCOL.md 4d): posted as the keyboard's own media keys,
// so whatever answers them (Music, Spotify, a browser's video) obeys. macOS asks for Accessibility access the
// first time. The Linux sender does the same through MPRIS.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit

public enum MediaKeys {
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
        for down in [true, false] {
            let flags = NSEvent.ModifierFlags(rawValue: down ? 0xa00 : 0xb00)
            let data1 = Int((code << 16) | ((down ? 0xa : 0xb) << 8))
            guard let e = NSEvent.otherEvent(with: .systemDefined, location: .zero, modifierFlags: flags, timestamp: 0,
                                             windowNumber: 0, context: nil, subtype: 8, data1: data1, data2: -1) else {
                return false
            }
            e.cgEvent?.post(tap: .cghidEventTap)
        }
        return true
    }
}
