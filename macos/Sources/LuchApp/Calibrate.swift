// "Calibrate with Microphone…" under a playing group, and the local API's `calibrate` (luchctl calibrate): each
// member whistles in turn, this Mac's microphone times them, and each gets the correction that makes all sound
// together (docs/research/sync-multiroom.md 6.7). The microphone is opened only then, and macOS asks only then.
// SPDX-License-Identifier: GPL-3.0-or-later
import AppKit
import LuchProto
import LuchEngine
import LuchFind
import LuchLocal

extension Together {
    /// Runs one calibration of the group that plays; blocks for its length (seconds per receiver).
    func calibrate() throws -> [String: Any] {
        guard let g = currentGroup() else { throw CalibrationError("no group is playing: `luchctl group` first") }
        guard calibrating.take() else { throw CalibrationError("a calibration is already running") }
        defer { calibrating.done() }
        show("Calibrating: keep the room quiet…")
        do {
            let calibrator = Calibrator(group: g, mic: Mic(), store: store)
            let results = try calibrator.run()
            // Kept for a look afterwards: the recording, and where each sweep was expected and found.
            let stamp = ISO8601DateFormatter().string(from: Date()).replacingOccurrences(of: ":", with: "-")
            let dir = Home.support.appendingPathComponent("calibration/\(stamp)")
            try? calibrator.save(to: dir)
            show("Playing")
            onMain(hooks.changed)
            return ["results": results.map(\.json)]
        } catch {
            show("Calibration: \(error.localizedDescription)")
            throw error
        }
    }

    /// The group's menu item.
    func calibrateItem(_ g: GroupSession) -> NSMenuItem {
        let item = NSMenuItem.run("Calibrate with Microphone…", symbol: "mic") { [weak self] in self?.askToCalibrate(g) }
        item.toolTip = "Each TV whistles in turn; this Mac's microphone times them so that all sound together"
        return item
    }

    private func askToCalibrate(_ g: GroupSession) {
        let alert = NSAlert()
        alert.messageText = "Calibrate with Microphone"
        let sounding = g.members.filter(\.role.audio).count
        alert.informativeText = CalibrateText.before(seconds: Calibrator.seconds(members: sounding)) + "\n\n" +
            "Each TV's sound is then set so that all are heard at the same moment where this Mac stands: put it " +
            "where you listen. macOS asks to use the microphone the first time."
        alert.addButton(withTitle: "Start")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let text: String
            do {
                let r = try self?.calibrate() ?? [:]
                text = CalibrateText.lines(r["results"] as? [[String: Any]] ?? []).joined(separator: "\n\n")
            } catch {
                text = error.localizedDescription
            }
            DispatchQueue.main.async {
                let done = NSAlert()
                done.messageText = "Calibration"
                done.informativeText = text
                NSApp.activate(ignoringOtherApps: true)
                done.runModal()
            }
        }
    }
}

/// One calibration at a time.
final class OneAtATime {
    private let lock = NSLock()
    private var busy = false

    /// True when it was free and is now taken.
    func take() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if busy { return false }
        busy = true
        return true
    }

    func done() {
        lock.lock()
        busy = false
        lock.unlock()
    }
}

/// The calibration's microphone: AVAudioEngine's input (LuchFind's recorder).
private final class Mic: CalibrationMic {
    private let recorder = MicRecorder()

    func start() throws { try recorder.start() }

    func recording() -> MicRecording {
        let r = recorder.snapshot()
        return MicRecording(samples: r.samples, rate: r.rate, anchors: r.anchors, captureLatency: AudioLatency.input)
    }

    func stop() { recorder.stop() }
}
