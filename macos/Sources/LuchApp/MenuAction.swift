// A menu item that runs a closure, so a menu built from state needs no selector per command.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit

final class MenuAction: NSObject {
    private let run: () -> Void
    init(_ run: @escaping () -> Void) { self.run = run }
    @objc func fire() { run() }
}

extension NSMenuItem {
    /// The item keeps its action alive through `representedObject` (a menu item's target is weak).
    static func run(_ title: String, enabled: Bool = true, symbol: String? = nil,
                    _ action: @escaping () -> Void) -> NSMenuItem {
        let a = MenuAction(action)
        let item = NSMenuItem(title: title, action: #selector(MenuAction.fire), keyEquivalent: "")
        item.target = a
        item.representedObject = a
        item.isEnabled = enabled
        if let symbol { item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil) }
        return item
    }

    static func label(_ title: String) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.isEnabled = false
        return item
    }
}
