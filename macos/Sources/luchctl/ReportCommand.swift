// luchctl report [--keep-names] [--send] [what happened…]: the app's "Report a Problem…" from a terminal, and with
// --send its "Send to the Developers". `report <n>` (a number) stays the receivers' crash report it was.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchLocal

enum ReportCommand {
    static let usage = "usage: luchctl report [--keep-names] [--send] [what happened…]"

    static func handles(_ args: [String]) -> Bool {
        args.first == "report" && !(args.count >= 2 && Int(args[1]) != nil)
    }

    /// Runs the command; the exit code.
    static func run(_ args: [String]) -> Int32 {
        var keep = false
        var send = false
        var words: [String] = []
        for a in args.dropFirst() {
            switch a {
            case "--keep-names": keep = true
            case "--send": send = true
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
            // Every receiver may take its 10 s, and they are asked at once; sending, a minute for each of its two parts.
            r = try LocalClient.request(["cmd": "make_report", "what": what, "keep_names": keep, "send": send],
                                        timeout: send ? 240 : 120)
        } catch {
            print("error: \(error.localizedDescription)")
            return 1
        }
        if let e = r["error"] as? String {
            print("error: \(e)")
            return 1
        }
        print(lines(r, kept: keep).joined(separator: "\n"))
        guard send else { return 0 }
        let out = sent(r)
        print(out.joined(separator: "\n"))
        return out.first?.hasPrefix("sent:") == true ? 0 : 1
    }

    /// The send's answer, as luchctl on Linux says it: the number to tell, or why not.
    static func sent(_ r: [String: Any]) -> [String] {
        if let e = r["send_error"] as? String { return ["not sent: \(e)"] }
        var out = ["sent: report number \(r["number"] as? String ?? "?") (tell it to us if you write in the chat)"]
        if let why = r["attach_error"] as? String {
            out.append("  the file itself did not go (\(why)): hand it over if we ask")
        }
        return out
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
