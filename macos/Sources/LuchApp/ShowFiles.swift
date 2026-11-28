// "Show on TV with Luch" and "Present on TV with Luch" from Finder: the Services of Info.plist's NSServices
// (Tools/make-app.sh) land here, and so do files dropped on the menu bar icon. One TV takes them at once; with
// several, a short menu under the pointer asks which (FileShow says what to offer).
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import LuchLocal
import LuchNet
import LuchTV

extension AppDelegate {
    /// Called once at launch with the menu bar icon.
    func setUpShowFiles(_ button: NSStatusBarButton?) {
        let service = ShowService { [weak self] files, verb in self?.showFiles(files, verbs: [verb]) }
        let drop = StatusDrop(accepts: FileShow.accepts) { [weak self] files in
            // A dropped presentation can be presented too: the menu then offers both.
            self?.showFiles(files, verbs: [.show, .present])
        }
        ShowFilesHold.service = service
        ShowFilesHold.drop = drop
        NSApp.servicesProvider = service
        // Under the NSPortName of Info.plist, whatever name AppKit would take from the bundle.
        NSRegisterServicesProvider(service, "Luch")
        NSUpdateDynamicServices()
        if let window = button?.window {
            window.registerForDraggedTypes([.fileURL])
            window.delegate = drop
            drop.button = button
        }
    }

    /// The receivers a file can go to: not the web receiver, which shows this screen only.
    private var showTargets: [ShowTarget] {
        receivers.filter { $0.service != "web" }.map {
            ShowTarget(id: $0.id, name: $0.name, presents: TVKind.of($0.id) == .luch)
        }
    }

    /// `tries`: a service may start the app, which has found nobody yet; it waits a few seconds for discovery.
    func showFiles(_ files: [String], verbs: [ShowVerb], tries: Int = 8) {
        switch FileShow.plan(verbs, files: files, receivers: showTargets, canWait: tries > 0) {
        case .wait:
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
                self?.showFiles(files, verbs: verbs, tries: tries - 1)
            }
        case .run(let c):
            run(c, files: files)
        case .ask(let choices):
            let menu = NSMenu()
            for c in choices {
                menu.addItem(.run(c.title, symbol: c.verb == .show ? "tv" : "play.rectangle") { [weak self] in
                    self?.run(c, files: files)
                })
            }
            NSApp.activate(ignoringOtherApps: true)
            menu.popUp(positioning: nil, at: NSEvent.mouseLocation, in: nil)
        case .tell(let text):
            tell(text)
        }
    }

    private func run(_ c: ShowChoice, files: [String]) {
        guard let r = receivers.first(where: { $0.id == c.receiver }) else { return }
        switch c.verb {
        case .show: send(FileShow.request(c, files: files), to: r)
        case .present: present(URL(fileURLWithPath: files[0]), on: r)
        }
    }

    private func tell(_ text: String) {
        let alert = NSAlert()
        alert.messageText = "Luch"
        alert.informativeText = text
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }
}

/// An extension cannot keep them, and AppKit keeps neither the services provider nor a window delegate.
private enum ShowFilesHold {
    static var service: ShowService?
    static var drop: StatusDrop?
}

/// NSServices' NSMessage names: showOnTV and presentOnTV, each `name:userData:error:`.
final class ShowService: NSObject {
    private let handle: ([String], ShowVerb) -> Void
    init(_ handle: @escaping ([String], ShowVerb) -> Void) { self.handle = handle }

    @objc func showOnTV(_ pboard: NSPasteboard, userData: String?, error: AutoreleasingUnsafeMutablePointer<NSString?>) {
        handle(Self.files(pboard), .show)
    }

    @objc func presentOnTV(_ pboard: NSPasteboard, userData: String?, error: AutoreleasingUnsafeMutablePointer<NSString?>) {
        handle(Self.files(pboard), .present)
    }

    static func files(_ pboard: NSPasteboard) -> [String] {
        let urls = pboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL]
        return (urls ?? []).map(\.path)
    }
}

/// The menu bar icon's window takes file drops: its delegate gets them (NSWindow passes a drag on to its delegate).
final class StatusDrop: NSObject, NSWindowDelegate, NSDraggingDestination {
    private let accepts: ([String]) -> Bool
    private let dropped: ([String]) -> Void
    weak var button: NSStatusBarButton?

    init(accepts: @escaping ([String]) -> Bool, dropped: @escaping ([String]) -> Void) {
        self.accepts = accepts
        self.dropped = dropped
    }

    private func files(_ info: NSDraggingInfo) -> [String] {
        let urls = info.draggingPasteboard.readObjects(forClasses: [NSURL.self],
                                                       options: [.urlReadingFileURLsOnly: true]) as? [URL]
        return (urls ?? []).map(\.path)
    }

    func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        guard accepts(files(sender)) else { return [] }
        button?.highlight(true)
        return .copy
    }

    func draggingExited(_ sender: NSDraggingInfo?) {
        button?.highlight(false)
    }

    func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        button?.highlight(false)
        let f = files(sender)
        guard accepts(f) else { return false }
        // After the drag has ended: a menu cannot open inside it.
        DispatchQueue.main.async { self.dropped(f) }
        return true
    }
}
