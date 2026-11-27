// "Report a Problem…" in the menu and `luchctl report`: a short form (what happened, hide addresses and names), then
// one zip in Downloads shown in Finder, with Copy Description, Open an Issue and, in a build with a report address,
// Send to the Developers (ProblemReport.swift and ReportSender.swift in LuchReport).
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
        var input = ProblemReport.Input(what: what, hide: hide, receivers: luch, inUse: inUse, otherNames: others, me: me,
                                        app: "Luch for macOS \(version) (\(build)), \(Proto.app)", logPath: logFile,
                                        downloads: downloads)
        input.version = version
        return input
    }

    /// `luchctl report [--send]`: answered on the socket's thread, as the receivers (and the sending) take a while.
    func handleReport(_ request: [String: Any]) -> [String: Any] {
        var input: ProblemReport.Input?
        DispatchQueue.main.sync {
            input = reportInput(what: request["what"] as? String ?? "", hide: request["keep_names"] as? Bool != true)
        }
        do {
            let r = try ProblemReport.make(input!)
            log("report: \(r.file.path)")
            let dsn = ReportSender.configured()
            var out: [String: Any] = ["ok": true, "file": r.file.path, "description": r.description, "issue": r.issueURL,
                                      "notes": r.notes, "can_send": dsn != nil]
            guard request["send"] as? Bool == true else { return out }
            guard let dsn else {
                out["send_error"] = "this build sends reports nowhere (REPORT_DSN when building it, or LUCH_REPORT_DSN)"
                return out
            }
            switch ReportSender.send(r, to: dsn) {
            case let .success(s):
                log("report sent: \(s.number)")
                out["number"] = s.number
                out["event_id"] = s.eventID
                out["attached"] = s.attachError == nil
                if let e = s.attachError { out["attach_error"] = e }
            case let .failure(e):
                out["send_error"] = e.localizedDescription
            }
            return out
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
        let dsn = ReportSender.configured()
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
            if dsn != nil { alert.addButton(withTitle: "Send to the Developers") }
            NSApp.activate(ignoringOtherApps: true)
            switch alert.runModal() {
            case .alertSecondButtonReturn:
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(r.description, forType: .string)
                copied = true
            case .alertThirdButtonReturn:
                if let url = URL(string: r.issueURL) { NSWorkspace.shared.open(url) }
                return
            case NSApplication.ModalResponse(NSApplication.ModalResponse.alertThirdButtonReturn.rawValue + 1):
                guard let dsn else { return }
                switch send(r, to: dsn) {
                case let .success(s):
                    showSent(s)
                    return
                case let .failure(e) where e.localizedDescription == ReportSender.stopped:
                    continue
                case let .failure(e):
                    let alert = NSAlert()
                    alert.messageText = "Could not send the report"
                    alert.informativeText = e.localizedDescription + "\n\nThe file is still in Downloads: drag it into "
                        + "a chat or an issue."
                    NSApp.activate(ignoringOtherApps: true)
                    alert.runModal()
                }
            default:
                return
            }
        }
    }

    // MARK: - sending

    /// Sends the report with a progress bar and Stop; the number, or why not (`ReportSender.stopped`).
    private func send(_ r: ProblemReport.Result, to dsn: ReportSender.DSN) -> Result<ReportSender.Sent, ReportError> {
        let alert = NSAlert()
        alert.messageText = "Sending the Report…"
        alert.informativeText = "\(r.file.lastPathComponent) goes to the developers' server."
        let bar = NSProgressIndicator(frame: NSRect(x: 0, y: 0, width: 300, height: 20))
        bar.style = .bar
        bar.isIndeterminate = false
        bar.minValue = 0
        bar.maxValue = 1
        alert.accessoryView = bar
        alert.addButton(withTitle: "Stop")
        var out: Result<ReportSender.Sent, ReportError> = .failure(ReportError(ReportSender.stopped))
        // Ends this alert only: an answer after Stop must not end the next one.
        var waiting = true
        let upload = ReportSender.Upload(r, to: dsn)
        upload.start(progress: { p in DispatchQueue.main.async { bar.doubleValue = p } }) { result in
            DispatchQueue.main.async {
                guard waiting else { return }
                out = result
                NSApp.abortModal()
            }
        }
        NSApp.activate(ignoringOtherApps: true)
        if alert.runModal() == .alertFirstButtonReturn { upload.cancel() }
        waiting = false
        return out
    }

    private func showSent(_ s: ReportSender.Sent) {
        log("report sent: \(s.number)")
        var copied = false
        while true {
            let alert = NSAlert()
            alert.messageText = "Sent"
            alert.informativeText = "Report number \(s.number) — tell it to us if you write in the chat."
                + (s.attachError == nil ? "" : "\n\nThe server did not take the file itself: send it if we ask.")
                + (copied ? "\n\nThe number is copied." : "")
            alert.addButton(withTitle: "Done")
            alert.addButton(withTitle: "Copy Number")
            NSApp.activate(ignoringOtherApps: true)
            guard alert.runModal() == .alertSecondButtonReturn else { return }
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(s.number, forType: .string)
            copied = true
        }
    }
}
