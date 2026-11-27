// Where the receiver's input lands on this Mac: a point of the picture as a point of the desktop, and a key of a PC
// keyboard (Linux evdev numbers) as the Mac key in the same place. Positions, not characters: the Mac's own input
// source makes the character, as a keyboard plugged into it would.
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreGraphics
import Foundation
import LuchMedia

/// The captured area in the desktop's global coordinates (points, top-left origin: what CGEvent and CGDisplayBounds
/// use), and where it sits in the video frame (fractions; less than the whole frame when it is letterboxed).
public struct InputGeometry: Equatable {
    public var area: CGRect
    public var content: CGRect

    public init(area: CGRect, content: CGRect = CGRect(x: 0, y: 0, width: 1, height: 1)) {
        self.area = area
        self.content = content
    }

    /// The frame's fractions that a `source`-shaped picture fitted into a `frame`-shaped one fills (bars around it).
    public static func letterbox(source: CGSize, frame: CGSize) -> CGRect {
        guard source.width > 0, source.height > 0, frame.width > 0, frame.height > 0 else {
            return CGRect(x: 0, y: 0, width: 1, height: 1)
        }
        let s = min(frame.width / source.width, frame.height / source.height)
        let w = source.width * s / frame.width, h = source.height * s / frame.height
        return CGRect(x: (1 - w) / 2, y: (1 - h) / 2, width: w, height: h)
    }

    /// The desktop point for a fraction of the video frame; a point on the bars lands on the picture's edge.
    public func point(_ x: Double, _ y: Double) -> CGPoint {
        let u = clamp((x - content.minX) / content.width), v = clamp((y - content.minY) / content.height)
        return CGPoint(x: area.minX + u * area.width, y: area.minY + v * area.height)
    }

    /// Keeps a point inside the area: relative motion does not take the pointer off the picture.
    public func clamped(_ p: CGPoint) -> CGPoint {
        CGPoint(x: min(max(p.x, area.minX), area.maxX - 1), y: min(max(p.y, area.minY), area.maxY - 1))
    }

    /// `area` of the grant: what the receiver scales relative motion by.
    public var areaSize: (Int, Int) { (Int(area.width.rounded()), Int(area.height.rounded())) }

    private func clamp(_ v: Double) -> Double { v.isNaN ? 0 : min(max(v, 0), 1) }
}

extension InputGeometry {
    /// For what the capture shows in a video of `frame` pixels: a window is asked where it is now.
    public static func of(_ shown: ScreenCapture.Shown?, frame: CGSize,
                          windowBounds: (CGWindowID) -> CGRect? = windowBounds) -> InputGeometry {
        switch shown {
        case let .area(r, letterboxed):
            return InputGeometry(area: r, content: letterboxed ? letterbox(source: r.size, frame: frame)
                                                                : CGRect(x: 0, y: 0, width: 1, height: 1))
        case let .window(id):
            let r = windowBounds(id) ?? .zero
            return InputGeometry(area: r, content: letterbox(source: r.size, frame: frame))
        case nil:
            return InputGeometry(area: CGDisplayBounds(CGMainDisplayID()))
        }
    }

    /// A window's frame in global points, from the window server.
    public static func windowBounds(_ id: CGWindowID) -> CGRect? {
        guard let info = CGWindowListCopyWindowInfo(.optionIncludingWindow, id) as? [[String: Any]],
              let bounds = info.first?[kCGWindowBounds as String] as? NSDictionary else { return nil }
        return CGRect(dictionaryRepresentation: bounds)
    }
}

public enum KeyMap {
    /// A Mac virtual key code (kVK_*) for a Linux evdev KEY_* number, by position on the keyboard.
    public static func macKey(_ evdev: UInt16) -> UInt16? { keys[evdev] }

    /// The media and system keys (NX_KEYTYPE_*): a Mac has no key code for these, they go as system events.
    public static func systemKey(_ evdev: UInt16) -> Int32? { system[evdev] }

    /// The flag a modifier key holds (Shift, Control, Option, Command, Fn); nil for the others.
    public static func modifier(_ macKey: UInt16) -> CGEventFlags? {
        switch macKey {
        case 0x38, 0x3C: return .maskShift
        case 0x3B, 0x3E: return .maskControl
        case 0x3A, 0x3D: return .maskAlternate
        case 0x37, 0x36: return .maskCommand
        case 0x3F: return .maskSecondaryFn
        default: return nil
        }
    }

    public static let capsLock: UInt16 = 0x39

    /// What a Mac keyboard itself puts on these keys: arrows are on the keypad and behind Fn, the navigation and
    /// F-keys behind Fn. Some apps (text fields' selection, games) look at it.
    public static func ownFlags(_ macKey: UInt16) -> CGEventFlags {
        switch macKey {
        case 0x7B...0x7E: return [.maskNumericPad, .maskSecondaryFn]
        case 0x72...0x79, 0x7A, 0x60...0x6F, 0x40, 0x4F, 0x50, 0x5A: return .maskSecondaryFn
        default: return []
        }
    }

    private static let keys: [UInt16: UInt16] = [
        1: 0x35,                                                         // Esc
        2: 0x12, 3: 0x13, 4: 0x14, 5: 0x15, 6: 0x17, 7: 0x16, 8: 0x1A, 9: 0x1C, 10: 0x19, 11: 0x1D, // 1..0
        12: 0x1B, 13: 0x18, 14: 0x33, 15: 0x30,                          // - = Backspace Tab
        16: 0x0C, 17: 0x0D, 18: 0x0E, 19: 0x0F, 20: 0x11, 21: 0x10, 22: 0x20, 23: 0x22, 24: 0x1F, 25: 0x23, // Q..P
        26: 0x21, 27: 0x1E, 28: 0x24, 29: 0x3B,                          // [ ] Enter LeftCtrl
        30: 0x00, 31: 0x01, 32: 0x02, 33: 0x03, 34: 0x05, 35: 0x04, 36: 0x26, 37: 0x28, 38: 0x25, // A..L
        39: 0x29, 40: 0x27, 41: 0x32, 42: 0x38, 43: 0x2A,                // ; ' ` LeftShift backslash
        44: 0x06, 45: 0x07, 46: 0x08, 47: 0x09, 48: 0x0B, 49: 0x2D, 50: 0x2E, // Z..M
        51: 0x2B, 52: 0x2F, 53: 0x2C, 54: 0x3C, 55: 0x43,                // , . / RightShift KP*
        56: 0x3A, 57: 0x31, 58: 0x39,                                    // LeftAlt (Option) Space CapsLock
        59: 0x7A, 60: 0x78, 61: 0x63, 62: 0x76, 63: 0x60, 64: 0x61, 65: 0x62, 66: 0x64, 67: 0x65, 68: 0x6D, // F1..F10
        69: 0x47, 70: 0x6B,                                              // NumLock (Clear) ScrollLock (F14)
        71: 0x59, 72: 0x5B, 73: 0x5C, 74: 0x4E, 75: 0x56, 76: 0x57, 77: 0x58, 78: 0x45, // KP7 KP8 KP9 KP- KP4..6 KP+
        79: 0x53, 80: 0x54, 81: 0x55, 82: 0x52, 83: 0x41,                // KP1..3 KP0 KP.
        86: 0x0A, 87: 0x67, 88: 0x6F, 89: 0x5E,                          // ISO key, F11, F12, Ro (JIS _)
        96: 0x4C, 97: 0x3E, 98: 0x4B, 99: 0x69, 100: 0x3D,               // KPEnter RightCtrl KP/ SysRq (F13) RightAlt
        102: 0x73, 103: 0x7E, 104: 0x74, 105: 0x7B, 106: 0x7C, 107: 0x77, 108: 0x7D, 109: 0x79, // Home Up PgUp ← → End ↓ PgDn
        110: 0x72, 111: 0x75, 117: 0x51, 119: 0x71, 121: 0x5F,           // Insert (Help) Delete KP= Pause (F15) KP,
        122: 0x68, 123: 0x66, 124: 0x5D,                                 // Hangeul (Kana) Hanja (Eisu) Yen
        125: 0x37, 126: 0x36, 127: 0x6E,                                 // LeftMeta RightMeta (Command) Compose (menu)
        183: 0x69, 184: 0x6B, 185: 0x71, 186: 0x6A, 187: 0x40, 188: 0x4F, 189: 0x50, 190: 0x5A, // F13..F20
        464: 0x3F,                                                       // Fn
    ]

    private static let system: [UInt16: Int32] = [
        113: 7, 114: 1, 115: 0,          // Mute, VolumeDown, VolumeUp
        161: 14,                         // Eject
        163: 17, 164: 16, 165: 18,       // NextSong, PlayPause, PreviousSong
        224: 3, 225: 2,                  // BrightnessDown, BrightnessUp
    ]
}
