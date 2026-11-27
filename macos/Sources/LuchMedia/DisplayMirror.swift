// This Mac's screen showing a copy of another (the TV's virtual screen) for as long as the object lives: the desktop is
// then drawn at the TV's mode, the way AirPlay's "match the TV" does it, and the Mac's panel shows it scaled.
// SPDX-License-Identifier: GPL-3.0-or-later
import CoreGraphics
import Foundation
import LuchProto

public final class DisplayMirror {
    private let mirror: CGDirectDisplayID

    /// `display` (this Mac's main screen) mirrors `master`. Only for this app: macOS undoes it if Luch quits or crashes.
    public init?(_ display: CGDirectDisplayID, of master: CGDirectDisplayID) {
        guard Self.configure(display, mirroring: master) else { return nil }
        mirror = display
        log("display: \(display) mirrors \(master)")
    }

    deinit {
        if Self.configure(mirror, mirroring: kCGNullDirectDisplay) { log("display: \(mirror) is its own again") }
    }

    private static func configure(_ display: CGDirectDisplayID, mirroring master: CGDirectDisplayID) -> Bool {
        var config: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&config) == .success, let config else { return false }
        let mirrored = CGConfigureDisplayMirrorOfDisplay(config, display, master)
        guard mirrored == .success else {
            CGCancelDisplayConfiguration(config)
            log("display: mirroring \(display) onto \(master) failed: \(mirrored.rawValue)")
            return false
        }
        let done = CGCompleteDisplayConfiguration(config, .forAppOnly)
        if done != .success { log("display: configuration failed: \(done.rawValue)") }
        return done == .success
    }
}
