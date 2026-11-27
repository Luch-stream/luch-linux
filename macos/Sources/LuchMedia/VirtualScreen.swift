// A second screen for this Mac that is really the TV: macOS lays windows on it like on a monitor, and Luch captures
// it like any display. Made with CoreGraphics' virtual displays, the private API Sidecar uses (CVirtualDisplay.h).
// SPDX-License-Identifier: GPL-3.0-or-later
import CVirtualDisplay
import CoreGraphics
import Foundation
import LuchProto

public final class VirtualScreen {
    public let displayID: CGDirectDisplayID
    /// The pixels macOS draws, what is captured and sent: the TV's own.
    public let pixelWidth: Int
    public let pixelHeight: Int
    private let display: CGVirtualDisplay

    /// What a TV of `width` x `height` pixels gets: from 2560 px wide it is Retina-like (hiDPI), the desktop in
    /// points at half the pixels, so a 4K TV reads like a 1080p screen and is twice as sharp.
    public static func layout(width: Int, height: Int) -> (hiDPI: Bool, pointsWide: Int, pointsHigh: Int) {
        let hi = width >= 2560
        return (hi, hi ? width / 2 : width, hi ? height / 2 : height)
    }

    /// `serial` keeps a TV the same screen to macOS from one session to the next, so it remembers where it was put.
    public init?(name: String, width: Int, height: Int, fps: Int, serial: UInt32) {
        let l = Self.layout(width: width, height: height)
        let d = CGVirtualDisplayDescriptor()
        d.name = name
        d.queue = DispatchQueue(label: "luch.virtual-screen")
        d.maxPixelsWide = UInt32(width)
        d.maxPixelsHigh = UInt32(height)
        // A 55-inch 16:9 panel: macOS takes its default scaling from the size.
        d.sizeInMillimeters = CGSize(width: 1210, height: 680)
        d.vendorID = 0x4C55 // "LU"
        d.productID = 1
        d.serialNum = serial
        guard let v = CGVirtualDisplay(descriptor: d) else {
            log("virtual screen: CoreGraphics made none")
            return nil
        }
        let s = CGVirtualDisplaySettings()
        s.hiDPI = l.hiDPI ? 1 : 0
        s.modes = [CGVirtualDisplayMode(width: UInt32(l.pointsWide), height: UInt32(l.pointsHigh),
                                        refreshRate: Double(fps))]
        guard v.apply(s) else {
            log("virtual screen: \(l.pointsWide)x\(l.pointsHigh)@\(fps) refused")
            return nil
        }
        display = v
        displayID = v.displayID
        pixelWidth = width
        pixelHeight = height
        log("virtual screen: \(displayID), \(l.pointsWide)x\(l.pointsHigh) points, \(width)x\(height) pixels @\(fps)")
    }

    /// Waits until macOS shows the screen with its mode (about a second after it is made); false after `timeout`.
    public func waitUntilShown(timeout: TimeInterval = 4) -> Bool {
        let until = Date().addingTimeInterval(timeout)
        while Date() < until {
            if let m = CGDisplayCopyDisplayMode(displayID), m.pixelWidth == pixelWidth { return true }
            Thread.sleep(forTimeInterval: 0.1) // the session starts off the main thread, which has no run loop to turn
        }
        return false
    }

    /// A stable serial for a receiver id.
    public static func serial(for id: String) -> UInt32 {
        var h: UInt32 = 2166136261 // FNV-1a
        for b in id.utf8 { h = (h ^ UInt32(b)) &* 16777619 }
        return h
    }

    deinit {
        log("virtual screen: \(displayID) removed")
    }
}
