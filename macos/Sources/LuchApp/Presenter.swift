// Presenting, as luchd does it: a document on the receiver with the presenter view on this Mac, the receiver's
// black screen, and do-not-disturb while presenting or mirroring (Sources/LuchPresent has the parts).
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import Foundation
import LuchCast
import LuchNet
import LuchPresent
import LuchProto
import PDFKit

final class Presenter {
    private let lock = NSLock()
    private var server: PresentServer?
    /// Each receiver's black screen as it last said (or as we last asked, while mirroring).
    private var blanks: [String: Bool] = [:]
    private var mirrorHold: AnyObject?
    private var mirrorGeneration = 0
    /// A 4h message over the mirroring session to that receiver; false when this Mac does not mirror to it.
    var mirrorSend: ((_ receiver: String, _ msg: [String: Any]) -> Bool)?
    /// The black screen changed (the menu follows it). Called on any thread.
    var onChange: (() -> Void)?

    private func views() throws -> PresentServer {
        lock.lock()
        defer { lock.unlock() }
        if let server { return server }
        let s = PresentServer()
        try s.start()
        server = s
        return s
    }

    /// Shows a PDF or a presentation on `r` and returns the presenter view's address (http://127.0.0.1:<port>/<token>/).
    func present(_ file: String, on r: Receiver, caster: Caster) throws -> String {
        guard Cast.kind(file) == .document, !file.hasPrefix("http://"), !file.hasPrefix("https://") else {
            throw CastError.bad("present takes a PDF or a presentation (.odp, .pptx...)")
        }
        let raw = file.hasPrefix("file://") ? (URL(string: file)?.path ?? file) : file
        let original = URL(fileURLWithPath: (raw as NSString).expandingTildeInPath).standardizedFileURL
        guard FileManager.default.isReadableFile(atPath: original.path) else { throw CastError.bad("\(file): not a file") }
        let name = original.lastPathComponent
        let out = FileManager.default.temporaryDirectory.appendingPathComponent("luch-doc", isDirectory: true)
        let pdf = try Cast.toPDF(original, outDir: out)
        // The notes come from the original: a PDF made by LibreOffice has none.
        let notes = Notes.speakerNotes(original: original, pdf: pdf)
        let session = try caster.session(r)
        _ = try session.cast([pdf.path], options: try CastOptions(json: ["title": name]), server: try caster.files())
        let pages = session.pages.pages > 0 ? session.pages.pages : PDFDocument(url: pdf)?.pageCount ?? 0
        let hold = Quiet.shared.hold(ask: Presenter.askQuiet)
        let show = Show(receiver: r.id, receiverName: r.name, name: name, pdf: pdf, notes: notes, ink: session.can("ink"),
                        page: session.pages.page, pages: pages, hold: hold) { [weak session] m in session?.send(m) }
        return try views().add(show)
    }

    /// The presenter view of what is presented on a receiver now.
    func viewURL(_ receiver: String) -> String? {
        lock.lock()
        let s = server
        lock.unlock()
        return s?.show(on: receiver)?.url
    }

    /// A pointer, ink or blank message for what is shown on `r`: on the channel that shows it (a cast, or this Mac's
    /// mirroring), as only that sender may draw on it.
    func annotate(_ msg: [String: Any], on r: Receiver, session: CastSession?) throws {
        guard let m = Annotate.sanitize(msg), m["t"] as? String != "doc_page" else {
            throw CastError.bad("not a pointer, ink or blank message")
        }
        if let session, session.showing != nil {
            session.send(m)
        } else if mirrorSend?(r.id, m) != true {
            throw CastError.bad("nothing is shown on \(r.name)")
        }
        if m["t"] as? String == "blank", let on = m["on"] as? Bool { setBlank(r.id, on) }
    }

    func blank(_ receiver: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return blanks[receiver] ?? false
    }

    private func setBlank(_ receiver: String, _ on: Bool) {
        lock.lock()
        let changed = (blanks[receiver] ?? false) != on
        blanks[receiver] = on
        lock.unlock()
        if changed { onChange?() }
    }

    /// What a receiver says on the cast channel: its page turns and black screen, and the end of what it shows.
    func heard(_ receiver: String, _ m: [String: Any]) {
        switch m["t"] as? String ?? "" {
        case "blank": setBlank(receiver, m["on"] as? Bool ?? false)
        case "doc_page", "stop": setBlank(receiver, false)
        default: break
        }
        lock.lock()
        let s = server
        lock.unlock()
        s?.show(on: receiver)?.show.heard(m)
    }

    // MARK: - do not disturb

    /// This Mac mirrors its screen (sound only does not count) or stopped: do-not-disturb follows it. Shortcuts take a
    /// moment, so it happens off the main thread.
    func mirroring(_ on: Bool) {
        lock.lock()
        mirrorGeneration += 1
        let generation = mirrorGeneration
        let old = mirrorHold
        mirrorHold = nil
        lock.unlock()
        DispatchQueue.global(qos: .utility).async { [weak self] in
            withExtendedLifetime(old) {}
            guard on, let self else { return }
            let hold = Quiet.shared.hold(ask: Presenter.askQuiet)
            self.lock.lock()
            if generation == self.mirrorGeneration { self.mirrorHold = hold }
            self.lock.unlock()
        }
    }

    /// `quiet` with no mode reads it; ask, on or off sets it (on with the shortcuts missing offers them).
    func quiet(_ mode: String?) -> [String: Any] {
        guard let mode else { return ["ok": true, "mode": Quiet.shared.mode.rawValue] }
        guard let m = QuietMode.parse(mode) else { return ["error": "ask, on or off"] }
        Quiet.save(m)
        var out: [String: Any] = ["ok": true, "mode": m.rawValue]
        if m == .on, !FocusShortcuts.ready {
            Presenter.offerShortcuts()
            out["note"] = "add the two shortcuts Shortcuts shows (\(FocusShortcuts.onName), \(FocusShortcuts.offName)): "
                + "macOS lets apps turn on Do Not Disturb only through them"
        }
        return out
    }

    /// Asks once while the app runs, as the GNOME extension asks once a login.
    static func askQuiet() {
        DispatchQueue.main.async {
            let alert = NSAlert()
            alert.messageText = "Hide notifications while presenting?"
            alert.informativeText = "Luch can turn on Do Not Disturb while it presents or mirrors this Mac's screen, and "
                + "turn it off afterwards. macOS lets apps do that only through Shortcuts: Luch adds two shortcuts "
                + "(\(FocusShortcuts.onName) and \(FocusShortcuts.offName)) the first time. "
                + "Settings: Luch menu → Do Not Disturb While Presenting."
            alert.addButton(withTitle: "Hide Notifications")
            alert.addButton(withTitle: "Keep Showing Them")
            NSApp.activate(ignoringOtherApps: true)
            let hide = alert.runModal() == .alertFirstButtonReturn
            DispatchQueue.global(qos: .utility).async {
                Quiet.shared.answer(hide)
                if hide, !FocusShortcuts.ready { offerShortcuts() }
            }
        }
    }

    /// Makes the two shortcuts and hands them to Shortcuts, which asks to add each; once both are there, a session
    /// already running goes quiet too.
    static func offerShortcuts() {
        DispatchQueue.global(qos: .utility).async {
            do {
                let dir = FileManager.default.temporaryDirectory.appendingPathComponent("luch-shortcuts", isDirectory: true)
                let files = try FocusShortcuts.makeFiles(in: dir)
                DispatchQueue.main.async { files.forEach { NSWorkspace.shared.open($0) } }
                for _ in 0..<60 {
                    Thread.sleep(forTimeInterval: 3)
                    if FocusShortcuts.ready {
                        Quiet.shared.setMode(Quiet.shared.mode)
                        return
                    }
                }
            } catch {
                log("quiet: \(error.localizedDescription)")
            }
        }
    }
}
