// "Report a Problem…" in the menu and `luchctl report`: a short form (what happened, hide addresses and names), then
// one zip in Downloads shown in Finder, with Copy Description and Open an Issue (ProblemReport.swift in LuchReport).
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import LuchEngine
import LuchNet
import LuchProto
import LuchReport
import LuchTV

/// A report is being made: the menu item waits for it.
private var reporting = false

extension AppDelegate {
    static let reportCommands: Set<String> = ["make_report"]

    func reportMenuItem() -> NSMenuItem {
        .run(reporting ? "Making the Report…" : "Report a Problem…", enabled: !reporting,
             symbol: "exclamationmark.bubble") { [weak self] in self?.askForReport() }
    }

    /// What the report needs from the app; on the main thread.
    func reportInput(what: String, hide: Bool) -> ProblemReport.Input {
        let inUse = Set(receivers.map(\.id).filter { $0 == currentReceiverID || caster.existing($0) != nil })
        let luch = receivers.filter { TVKind.of($0.id) == .luch && $0.service != "web" }
            .sorted { inUse.contains($0.id) && !inUse.contains($1.id) }
        // TVs without Luch and browsers are not asked, but their names are hidden too.
        let others = receivers.filter { TVKind.of($0.id) != .luch || $0.service == "web" }.map(\.name)
            + finding.finder.known.load().values.compactMap { $0["name"] as? String }
            + Connector.shared.paired().compactMap { $0["name"] as? String }
        let info = Bundle.main.infoDictionary ?? [:]
        let version = info["CFBundleShortVersionString"] as? String ?? "?"
        let build = info["CFBundleVersion"] as? String ?? "?"
        let me = senderIdentity()
        let downloads = FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Downloads")
        return ProblemReport.Input(what: what, hide: hide, receivers: luch, inUse: inUse, otherNames: others, me: me,
                                   app: "Luch for macOS \(version) (\(build)), \(Proto.app)", logPath: logFile,
                                   downloads: downloads)
    }

    /// `luchctl report`: answered on the socket's thread, as the receivers take a while.
    func handleReport(_ request: [String: Any]) -> [String: Any] {
        var input: ProblemReport.Input?
        DispatchQueue.main.sync {
            input = reportInput(what: request["what"] as? String ?? "", hide: request["keep_names"] as? Bool != true)
        }
        do {
            let r = try ProblemReport.make(input!)
            log("report: \(r.file.path)")
            return ["ok": true, "file": r.file.path, "description": r.description, "issue": r.issueURL, "notes": r.notes]
        } catch {
            return ["error": error.localizedDescription]
        }
    }

    // MARK: - the form

    private func askForReport() {
        let alert = NSAlert()
        alert.messageText = "Report a Problem"
        alert.informativeText = "Luch puts this Mac's log, the logs of the TVs paired with it and what you write here "
            + "into one file in Downloads. Nothing is sent: you choose where the file goes (a chat, an issue)."
        let box = NSView(frame: NSRect(x: 0, y: 0, width: 340, height: 150))
        let label = NSTextField(labelWithString: "What happened?")
        label.frame = NSRect(x: 0, y: 130, width: 340, height: 18)
        box.addSubview(label)
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 30, width: 340, height: 96))
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        let text = NSTextView(frame: scroll.contentView.bounds)
        text.autoresizingMask = [.width]
        text.isRichText = false
        text.font = .systemFont(ofSize: NSFont.systemFontSize)
        scroll.documentView = text
        box.addSubview(scroll)
        let hide = NSButton(checkboxWithTitle: "Hide addresses and names", target: nil, action: nil)
        hide.state = .on
        hide.frame = NSRect(x: 0, y: 2, width: 340, height: 20)
        box.addSubview(hide)
        alert.accessoryView = box
        alert.addButton(withTitle: "Make Report")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = text
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }

        let input = reportInput(what: text.string.trimmingCharacters(in: .whitespacesAndNewlines), hide: hide.state == .on)
        reporting = true
        refreshReceivers()
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let made = Result { try ProblemReport.make(input) }
            DispatchQueue.main.async {
                reporting = false
                self?.refreshReceivers()
                self?.showReport(made)
            }
        }
    }

    private func showReport(_ made: Result<ProblemReport.Result, Error>) {
        let r: ProblemReport.Result
        switch made {
        case let .success(done): r = done
        case let .failure(error):
            let alert = NSAlert()
            alert.messageText = "Could not make the report"
            alert.informativeText = error.localizedDescription
            NSApp.activate(ignoringOtherApps: true)
            alert.runModal()
            return
        }
        log("report: \(r.file.path)")
        NSWorkspace.shared.activateFileViewerSelecting([r.file])
        var copied = false
        while true {
            let alert = NSAlert()
            alert.messageText = "The report is in Downloads"
            alert.informativeText = "\(r.file.lastPathComponent) is shown in Finder: drag it into a chat or an issue."
                + (r.notes.isEmpty ? "" : "\n\n" + r.notes.joined(separator: "\n"))
                + (copied ? "\n\nThe description is copied." : "")
            alert.addButton(withTitle: "Done")
            alert.addButton(withTitle: "Copy Description")
            alert.addButton(withTitle: "Open an Issue")
            NSApp.activate(ignoringOtherApps: true)
            switch alert.runModal() {
            case .alertSecondButtonReturn:
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(r.description, forType: .string)
                copied = true
            case .alertThirdButtonReturn:
                if let url = URL(string: r.issueURL) { NSWorkspace.shared.open(url) }
                return
            default:
                return
            }
        }
    }
}
