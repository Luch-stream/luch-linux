// luchctl report [--keep-names] [what happened…]: the app's "Report a Problem…" from a terminal. `report <n>` (a
// number) stays the receivers' crash report it was.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchLocal

enum ReportCommand {
    static let usage = "usage: luchctl report [--keep-names] [what happened…]"

    static func handles(_ args: [String]) -> Bool {
        args.first == "report" && !(args.count >= 2 && Int(args[1]) != nil)
    }

    /// Runs the command; the exit code.
    static func run(_ args: [String]) -> Int32 {
        var keep = false
        var words: [String] = []
        for a in args.dropFirst() {
            switch a {
            case "--keep-names": keep = true
            case "-h", "--help":
                print(usage)
                return 0
            default:
                if a.hasPrefix("--") {
                    print(usage)
                    return 2
                }
                words.append(a)
            }
        }
        var what = words.joined(separator: " ")
        if what.isEmpty, isatty(0) != 0 {
            print("What happened? ", terminator: "")
            what = readLine() ?? ""
        }
        print("collecting the logs of this Mac and of its receivers…")
        let r: [String: Any]
        do {
            // Every receiver may take its 10 s, and they are asked at once.
            r = try LocalClient.request(["cmd": "make_report", "what": what, "keep_names": keep], timeout: 120)
        } catch {
            print("error: \(error.localizedDescription)")
            return 1
        }
        if let e = r["error"] as? String {
            print("error: \(e)")
            return 1
        }
        print(lines(r, kept: keep).joined(separator: "\n"))
        return 0
    }

    static func lines(_ r: [String: Any], kept: Bool) -> [String] {
        var out = ["report: \(r["file"] as? String ?? "?")"]
        out += (r["notes"] as? [String] ?? []).map { "  " + $0 }
        out.append(kept ? "addresses and names kept as they are" : "addresses and names hidden (--keep-names keeps them)")
        out += ["", "for a chat:", (r["description"] as? String ?? "").trimmingCharacters(in: .newlines), "",
                "an issue (attach the file): \(r["issue"] as? String ?? "")"]
        return out
    }
}
