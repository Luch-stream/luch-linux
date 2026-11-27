// Ultra-low latency the AirPlay way: the Mac draws its desktop at exactly the receiver's mode (a virtual screen it
// mirrors onto), so nothing is scaled or converted on either side. Which of the receiver's modes, the person says
// once per TV.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum UltraPriority: String, CaseIterable {
    /// The most pixels the TV shows and decodes, at its fastest rate for that size.
    case quality
    /// The size nearest this Mac's own panel, at the TV's fastest rate for it.
    case likeMac = "mac"
    /// 1080p (or the nearest) at the fastest rate the TV has: 60, 120, 240.
    case speed

    public var label: String {
        switch self {
        case .quality: return "Quality: the TV's sharpest picture"
        case .likeMac: return "Like this Mac: the nearest size to its screen"
        case .speed: return "Speed: 1080p at the TV's highest refresh rate"
        }
    }
}

public enum UltraMode {
    /// The mode to draw at: one of the TV's own (a mode it does not list would be scaled again), within what its
    /// decoder takes; `macPixelWidth` is this Mac's panel. Without a list, the TV's current mode.
    public static func choose(_ priority: UltraPriority, caps: ReceiverCaps, macPixelWidth: Int) -> DisplayMode? {
        let maxW = caps.video.values.map(\.maxWidth).max() ?? 0
        let maxH = caps.video.values.map(\.maxHeight).max() ?? 0
        var modes = caps.displayModes.filter { m in
            (maxW == 0 || m.width <= maxW) && (maxH == 0 || m.height <= maxH) && m.width >= 1280
        }
        if modes.isEmpty, caps.renderWidth > 0, caps.renderHeight > 0 {
            modes = [DisplayMode(width: caps.renderWidth, height: caps.renderHeight, hz: max(30, caps.refreshRate))]
        }
        guard !modes.isEmpty else { return nil }
        // Of one size the fastest rate; then the size by priority.
        func fastest(_ ms: [DisplayMode]) -> DisplayMode? { ms.max { $0.hz < $1.hz } }
        switch priority {
        case .quality:
            let most = modes.map { $0.width * $0.height }.max()!
            return fastest(modes.filter { $0.width * $0.height == most })
        case .likeMac:
            let near = modes.map { abs($0.width - macPixelWidth) }.min()!
            return fastest(modes.filter { abs($0.width - macPixelWidth) == near })
        case .speed:
            let near = modes.map { abs($0.height - 1080) }.min()!
            return fastest(modes.filter { abs($0.height - 1080) == near })
        }
    }
}
