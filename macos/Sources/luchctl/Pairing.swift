// luchctl pair, paired, forget, reports and report: the same words as luchctl on Linux. A command that finds a
// receiver asking for pairing asks for its code here, then goes on.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import LuchLocal

private func request(_ r: [String: Any], timeout: TimeInterval = 10) -> [String: Any] {
    (try? LocalClient.request(r, timeout: timeout)) ?? ["error": "Luch.app is not running"]
}

private func failed(_ why: String) -> Never {
    print("error: \(why)")
    exit(1)
}

/// `luchctl pair [r] [code]`: connects (the TV shows a code, maybe after its person allows it), asks for the code
/// unless given, and hands it to the app. Already paired: says so.
func pairWith(_ name: String?, code: String?) {
    let r = callPicking(withReceiver(["cmd": "connect"], name), timeout: 15, askCode: false)
    guard let error = r["error"] as? String else {
        let encrypted = (r["encrypted"] as? Bool) == true
        if !encrypted {
            print("connected to an older receiver: it has no pairing and no encryption")
        } else {
            print((r["already"] as? Bool) == true ? "already paired and connected"
                                                  : "already paired: the receiver lets this computer in")
        }
        return
    }
    guard let id = pairingID(error) else { failed(error) }
    if let why = enterCode(id, code: code) { failed(why) }
}

/// The receiver `id` asks for pairing: waits for the TV's allow, asks for the code unless given and pairs. Returns
/// why not, or nil when paired.
func enterCode(_ id: String, code: String?) -> String? {
    var told = false
    let until = Date().addingTimeInterval(70)
    var name = "the receiver"
    waiting: while true {
        let st = request(["cmd": "pairing", "id": id])["state"] as? [String: Any] ?? [:]
        name = st["name"] as? String ?? name
        switch st["step"] as? String {
        case "code":
            break waiting
        case "allow" where !told:
            print("\(name) is asking its viewers first: allow the connection on the TV")
            told = true
        case "failed":
            return st["reason"] as? String ?? "pairing failed"
        case "paired":
            return nil
        default:
            break
        }
        if Date() > until { return "nobody answered on the TV" }
        usleep(300_000)
    }
    var typed = code ?? ""
    if code == nil {
        print("Code shown on \(name): ", terminator: "")
        fflush(stdout)
        typed = (readLine() ?? "").trimmingCharacters(in: .whitespaces)
    }
    let r = request(["cmd": "pair", "id": id, "code": typed], timeout: 30)
    if let error = r["error"] as? String { return error }
    print("paired with \(name): from now on it lets this computer in")
    return nil
}

/// A refusal that asks for pairing, on a terminal: the code is typed here, then the request is sent again.
func pairedAndAgain(_ request: [String: Any], _ response: [String: Any], timeout: TimeInterval) -> [String: Any] {
    guard let id = (response["error"] as? String).flatMap(pairingID), isatty(STDIN_FILENO) != 0 else { return response }
    if let why = enterCode(id, code: nil) { return ["error": why] }
    return (try? LocalClient.request(request, timeout: timeout)) ?? ["error": "Luch.app is not running"]
}

func listPaired() {
    let all = request(["cmd": "paired"])["paired"] as? [[String: Any]] ?? []
    if all.isEmpty { print("(not paired with any receiver yet: `luchctl pair`)") }
    func pad(_ s: String, _ n: Int) -> String { s + String(repeating: " ", count: max(0, n - s.count)) }
    for r in all {
        print("\(pad(r["id"] as? String ?? "", 38)) \(pad(r["name"] as? String ?? "", 20)) \((r["fp"] as? String ?? "").prefix(16))")
    }
}

func forget(_ key: String) {
    let r = request(["cmd": "forget", "name": key])
    if let error = r["error"] as? String { failed(error) }
    print("forgotten: the next connection pairs again with a new code")
}

func listReports() {
    let all = request(["cmd": "reports"])["reports"] as? [[String: Any]] ?? []
    if all.isEmpty { print("no reports") }
    for (n, r) in all.enumerated() {
        let count = (r["count"] as? NSNumber)?.intValue ?? 1
        print("\(n + 1). \(r["receiver_model"] as? String ?? r["device"] as? String ?? "?") \(r["kind"] as? String ?? "?"): "
            + "\(r["what"] as? String ?? "?")\(count > 1 ? " (\(count) times)" : "")")
    }
}

/// `luchctl report <n> [open|dismiss]`: shows it, opens the issue to file it, or forgets it.
func report(_ n: String, _ action: String?) {
    let all = request(["cmd": "reports"])["reports"] as? [[String: Any]] ?? []
    guard let i = Int(n), i >= 1, i <= all.count else { failed("no report \(n) (see `luchctl reports`)") }
    let r = all[i - 1]
    let id = r["id"] as? String ?? ""
    switch action {
    case nil:
        if let d = try? JSONSerialization.data(withJSONObject: r, options: [.prettyPrinted, .sortedKeys]) {
            print(String(decoding: d, as: UTF8.self))
        }
    case "open":
        let url = request(["cmd": "report_url", "id": id])["url"] as? String ?? ""
        let open = Process()
        open.executableURL = URL(fileURLWithPath: "/usr/bin/open")
        open.arguments = [url]
        if url.isEmpty || (try? open.run()) == nil { print(url) }
    case "dismiss":
        if let error = request(["cmd": "dismiss_report", "id": id])["error"] as? String { failed(error) }
    case let other?:
        failed("report \(n) open|dismiss, not \(other)")
    }
}
