// What a terminal needs of Luch, put in place at every launch, so nobody runs a setup command: `luchctl` on the PATH
// (Homebrew's bin, a link into this bundle) and each shell told that luchctl completes its own words (the shells
// ask `luchctl __complete`; this only registers it, as a package manager would). Nothing is overwritten that is not
// ours, and a file is written only when it changed.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchProto

enum ShellSetup {
    static func run() {
        guard let luchctl = Bundle.main.executableURL?.deletingLastPathComponent().appendingPathComponent("luchctl"),
              FileManager.default.isExecutableFile(atPath: luchctl.path) else { return }
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let brew = ["/opt/homebrew", "/usr/local"].first { FileManager.default.isWritableFile(atPath: $0 + "/bin") }
        if let brew { link(luchctl.path, at: brew + "/bin/luchctl") }

        var places: [(shell: String, file: String)] = []
        if let brew { places.append(("zsh", brew + "/share/zsh/site-functions/_luchctl")) }
        places.append(("bash", home + "/.local/share/bash-completion/completions/luchctl"))
        if FileManager.default.fileExists(atPath: home + "/.config/fish") {
            places.append(("fish", home + "/.config/fish/completions/luchctl.fish"))
        }
        for p in places {
            guard let script = output(luchctl, ["completion", p.shell]) else { continue }
            write(script, to: p.file)
        }
    }

    /// A link to this bundle's luchctl, unless something else of the same name is there.
    private static func link(_ target: String, at path: String) {
        let fm = FileManager.default
        if let current = try? fm.destinationOfSymbolicLink(atPath: path) {
            guard current != target, current.hasSuffix("Luch.app/Contents/MacOS/luchctl") else { return }
            try? fm.removeItem(atPath: path)
        } else if fm.fileExists(atPath: path) {
            return
        }
        do {
            try fm.createSymbolicLink(atPath: path, withDestinationPath: target)
            log("shell: \(path) → \(target)")
        } catch {
            log("shell: cannot link \(path): \(error.localizedDescription)")
        }
    }

    private static func write(_ text: String, to path: String) {
        let url = URL(fileURLWithPath: path)
        if let old = try? String(contentsOf: url, encoding: .utf8), old == text { return }
        do {
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try text.write(to: url, atomically: true, encoding: .utf8)
            log("shell: completion written to \(path)")
        } catch {
            log("shell: cannot write \(path): \(error.localizedDescription)")
        }
    }

    private static func output(_ exe: URL, _ args: [String]) -> String? {
        let p = Process()
        p.executableURL = exe
        p.arguments = args
        let pipe = Pipe()
        p.standardOutput = pipe
        guard (try? p.run()) != nil else { return nil }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        return p.terminationStatus == 0 ? String(decoding: data, as: UTF8.self) : nil
    }
}
