// luchctl — CLI over Luch.app's local socket.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchLocal

func call(_ request: [String: Any]) -> [String: Any] {
    do {
        return try LocalClient.request(request)
    } catch {
        FileHandle.standardError.write(Data("luchctl: \(error.localizedDescription)\n".utf8))
        exit(1)
    }
}

let args = Array(CommandLine.arguments.dropFirst())
if NativeHost.startedByBrowser(args) { NativeHost.run() }
if args.first == "__complete" { Complete.run(Array(args.dropFirst())) }
if args.first == "completion" {
    guard args.count == 2, let script = Complete.script(args[1]) else {
        print("usage: luchctl completion zsh|bash|fish   (zsh: luchctl completion zsh > \"${fpath[1]}/_luchctl\")")
        exit(2)
    }
    print(script)
    exit(0)
}
let command = args.first ?? "list"

func fail(_ response: [String: Any]) {
    if let error = response["error"] as? String {
        print("error: \(error)")
        exit(1)
    }
}

/// `[receiver] rest`: the first word is a receiver's name or id when more words follow and it is not a file or link.
func receiverAnd(_ words: [String], isWord: (String) -> Bool) -> (String?, [String]) {
    guard let first = words.first, words.count > 1 || !isWord(first) else { return (nil, words) }
    if isWord(first) { return (nil, words) }
    return (first, Array(words.dropFirst()))
}

func withReceiver(_ request: [String: Any], _ name: String?) -> [String: Any] {
    var r = request
    if let name {
        r["name"] = name
        r["id"] = name
    }
    return r
}

/// Sends a request; when it named no receiver and the app cannot tell which one is meant, asks on the terminal and
/// sends it again to the one picked. A script without a terminal gets the app's error, which names them. A receiver
/// that asks for pairing gets its code typed here (`askCode`), and the request goes again.
func callPicking(_ request: [String: Any], timeout: TimeInterval = 10, askCode: Bool = true) -> [String: Any] {
    var response = (try? LocalClient.request(request, timeout: timeout)) ?? ["error": "Luch.app is not running"]
    if request["id"] == nil, (response["error"] as? String)?.hasPrefix("several receivers") == true,
       let picked = chooseReceiver() {
        var again = request
        again["id"] = picked
        response = (try? LocalClient.request(again, timeout: timeout)) ?? ["error": "Luch.app is not running"]
    }
    return askCode ? pairedAndAgain(request, response, timeout: timeout) : response
}

func chooseReceiver() -> String? {
    let all = (call(["cmd": "receivers"])["receivers"] as? [[String: Any]] ?? []).compactMap { r -> ReceiverEntry? in
        guard let id = r["id"] as? String, let name = r["name"] as? String else { return nil }
        return ReceiverEntry(id: id, name: name)
    }
    guard all.count > 1 else { return nil }
    return ReceiverChoice.ask(all)?.id
}

if FindCommands.handles(args) { exit(FindCommands.run(args)) }
if DirectCommands.handles(args) { exit(DirectCommands.run(args)) }
if ReportCommand.handles(args) { exit(ReportCommand.run(args)) }

switch command {
case "list":
    let response = call(["cmd": "receivers"])
    let receivers = response["receivers"] as? [[String: Any]] ?? []
    if receivers.isEmpty {
        print("no receivers found (Luch app on the TV? same LAN?)")
    }
    let rows = receivers.map { receiver -> [String] in
        let mirroring = (receiver["mirroring"] as? Bool) == true ? " *" : ""
        let name = receiver["name"] as? String ?? "?"
        let address = receiver["address"] as? String ?? "?"
        let port = (receiver["port"] as? NSNumber)?.intValue ?? 0
        let iface = receiver["iface"] as? String ?? "-"
        let model = receiver["model"] as? String ?? ""
        // luch, cast or dlna: how the TV is reached (a TV without the Luch app plays files and links only).
        let kind = receiver["kind"] as? String ?? "luch"
        return ["\(name)\(mirroring)", "\(address):\(port)", iface.isEmpty ? "-" : iface, kind, model]
    }
    if !rows.isEmpty { Table.print(header: ["RECEIVER", "ADDRESS", "NET", "KIND", "MODEL"], rows: rows) }

case "mirror":
    var request: [String: Any] = ["cmd": "mirror"]
    var positionals: [String] = []
    var index = 1
    while index < args.count {
        if args[index] == "--codec", index + 1 < args.count {
            request["codec"] = args[index + 1]
            index += 2
            continue
        }
        if (args[index] == "--preset" || args[index] == "--quality"), index + 1 < args.count {
            request["preset"] = args[index + 1]
            index += 2
            continue
        }
        if args[index] == "--ultra", index + 1 < args.count {
            request["ultra"] = args[index + 1]
            index += 2
            continue
        }
        if args[index] == "--extend" {
            request["extend"] = true
            index += 1
            continue
        }
        if args[index] == "--size-mode", index + 1 < args.count {
            request["size_mode"] = args[index + 1]
            index += 2
            continue
        }
        if args[index] == "--profile", index + 1 < args.count {
            request["profile"] = args[index + 1]
            index += 2
            continue
        }
        if args[index] == "--frame-ms" {
            if index + 1 < args.count, let frameMs = Int(args[index + 1]) {
                request["frame_ms"] = frameMs
            }
            index += 2
            continue
        }
        positionals.append(args[index])
        index += 1
    }
    let (name, source, which) = MirrorSource.split(positionals) { word in
        (call(["cmd": "receivers"])["receivers"] as? [[String: Any]] ?? []).contains {
            ($0["name"] as? String)?.lowercased() == word.lowercased() || $0["id"] as? String == word
        }
    }
    if let name { request["name"] = name }
    if let source {
        guard let parsed = MirrorSource.parse(source) else {
            print("usage: luchctl mirror [r] [fast|monitor|window|virtual|audio|asis|extend|speed|quality|mac] [which]")
            exit(2)
        }
        request["source"] = source
        if let which { request["which"] = which }
        // No display or window named, several there: picked here, on the terminal.
        if which == nil, parsed == .monitor(nil) || parsed == .window(nil) {
            let what = parsed == .window(nil) ? "windows" : "displays"
            let all = (call(["cmd": what])[what] as? [[String: Any]] ?? []).compactMap { e -> ReceiverEntry? in
                guard let id = e["id"] as? String, let name = e["name"] as? String else { return nil }
                return ReceiverEntry(id: id, name: name)
            }
            if all.count > 1, let picked = SourceChoice.ask(String(what.dropLast()), all) {
                request["which"] = "#" + picked.id
            }
        }
    }
    let response = call(request)
    if let error = response["error"] as? String { print("error: \(error)"); exit(1) }
    if let asking = response["asking"] as? String { print(asking) }
    print("mirroring to \(response["receiver"] as? String ?? "?")")
    codeIfAskedWhileMirroring(response["receiver"] as? String ?? "")

case "stats":
    let response = call(["cmd": "stats"])
    let stats = response["stats"] as? [String: Any] ?? [:]
    if stats.isEmpty { print("not streaming") }
    else {
        let est = (stats["est_ms"] as? NSNumber)?.doubleValue ?? 0
        let host = (stats["host_ms"] as? NSNumber)?.doubleValue ?? 0
        let rtt = (stats["rtt_ms"] as? NSNumber)?.doubleValue ?? 0
        let decode = (stats["decode_ms"] as? NSNumber)?.doubleValue
        let fps = (stats["fps"] as? NSNumber)?.doubleValue ?? 0
        let mbps = (stats["mbps"] as? NSNumber)?.doubleValue ?? 0
        var line = String(format: "≈ %.0f ms · host %.1f · rtt %.1f · %.0f fps · %.1f Mbit/s", est, host, rtt, fps, mbps)
        if let decode { line += String(format: " · decode %.1f", decode) }
        if let battery = (stats["battery"] as? NSNumber)?.intValue {
            line += " · phone \(battery)%"
            if (stats["charging"] as? NSNumber)?.boolValue == true { line += "⚡" }
        }
        if let thermal = stats["thermal"] as? String, thermal != "none" { line += " · \(thermal)" }
        print(line)
    }
    printGroup()

case "group":
    groupCommand(args)

case "offset":
    offsetCommand(args)

case "calibrate":
    calibrateCommand(args)

case "audio":
    audioCommand(args)

case "latency":
    latencyCommand(args)

case "state":
    let response = call(["cmd": "state"])
    print("state: \(response["state"] as? String ?? "?"), streaming: \((response["streaming"] as? Bool) == true)")

case "screen":
    guard args.count > 1 else { print("usage: luchctl screen asis|speed|quality|mac|extend"); exit(2) }
    fail(call(["cmd": "screen", "mode": args[1]]))

case "warmth":
    guard args.count > 1, let value = Double(args[1]) else {
        print("usage: luchctl warmth 0...1")
        exit(2)
    }
    let response = call(["cmd": "warmth", "value": value])
    if let error = response["error"] as? String { print("error: \(error)"); exit(1) }
    print(String(format: "warmth: %.2f", (response["warmth"] as? NSNumber)?.doubleValue ?? value))

case "stop":
    let response = call(["cmd": "stop"])
    if let error = response["error"] as? String { print("error: \(error)"); exit(1) }
    print("stopped")

case "cast":
    var items: [String] = []
    var options: [String: Any] = [:]
    var subs: [String] = []
    var i = 1
    var name: String?
    while i < args.count {
        let a = args[i]
        func value() -> String { i += 1; return i < args.count ? args[i] : "" }
        switch a {
        case "--title": options["title"] = value()
        case "--start": options["start"] = Double(value().split(separator: ":").reduce(0.0) { $0 * 60 + (Double($1) ?? 0) })
        case "--sub": subs.append(URL(fileURLWithPath: value()).standardizedFileURL.path)
        case "--relay": options["via"] = "sender"
        case "--paused": options["paused"] = true
        case "--to": name = value()
        default:
            items.append(a.contains("://") ? a : URL(fileURLWithPath: (a as NSString).expandingTildeInPath).standardizedFileURL.path)
        }
        i += 1
    }
    if !subs.isEmpty { options["subs"] = subs }
    guard !items.isEmpty else { print("usage: luchctl cast <file|link>... [--to receiver] [--title T] [--start m:ss] [--sub file] [--relay]"); exit(2) }
    let response = callPicking(withReceiver(["cmd": "cast", "items": items, "options": options], name), timeout: 60)
    fail(response)
    switch response["kind"] as? String ?? "" {
    case "media": print("playing on the receiver; `luchctl media` follows it")
    case "doc": print("the document is on the receiver; `luchctl page next` turns its pages")
    case "app": print("opened in the TV's own app")
    default: print("on the receiver")
    }

case "media":
    let words = Array(args.dropFirst())
    let verbs: Set<String> = ["play", "pause", "toggle", "seek", "skip", "rate", "audio", "sub", "state"]
    let (name, rest) = receiverAnd(words) { verbs.contains($0) }
    if rest.isEmpty {
        let r = callPicking(withReceiver(["cmd": "media_state"], name))
        fail(r)
        let s = r["state"] as? [String: Any] ?? [:]
        print(s.isEmpty ? "(nothing playing)" : "\(s["state"] ?? "?") \(s["pos"] ?? 0) / \(s["dur"] ?? "-")")
        exit(0)
    }
    var cmd: [String: Any] = ["do": rest[0]]
    let arg = rest.count > 1 ? rest[1] : ""
    switch rest[0] {
    case "seek": cmd["pos"] = arg.split(separator: ":").reduce(0.0) { $0 * 60 + (Double($1) ?? 0) }
    case "skip": cmd["by"] = Double(arg) ?? 10
    case "rate": cmd["rate"] = Double(arg) ?? 1
    case "audio": cmd["id"] = arg
    case "sub": cmd["id"] = arg == "off" ? NSNull() : arg as Any
    default: break
    }
    fail(callPicking(withReceiver(["cmd": "media_cmd", "media": cmd], name)))

case "sleep":
    fail(callPicking(withReceiver(["cmd": "sleep"], args.count > 1 ? args[1] : nil)))

case "volume":
    let words = Array(args.dropFirst())
    let (name, rest) = receiverAnd(words) { ["+", "-", "up", "down", "mute", "unmute"].contains($0) || Int($0) != nil }
    let r = callPicking(withReceiver(["cmd": "volume", "what": rest.first ?? ""], name))
    fail(r)
    let v = r["volume"] as? [String: Any] ?? [:]
    print("\(v["level"] ?? "?")%\((v["muted"] as? Bool) == true ? " (muted)" : "")")

case "page":
    let words = Array(args.dropFirst())
    let (name, rest) = receiverAnd(words) { ["next", "prev", "+", "-"].contains($0) || Int($0) != nil }
    let r = callPicking(withReceiver(["cmd": "page", "what": rest.first ?? "next"], name))
    fail(r)
    print(r["page"] as? String ?? "")

case "present" where (2...3).contains(args.count):
    // A PDF or a presentation on the receiver, the presenter view here (current and next slide, notes, timer).
    let (name, file) = args.count == 3 ? (args[1], args[2]) : (nil, args[1])
    let path = URL(fileURLWithPath: (file as NSString).expandingTildeInPath).standardizedFileURL.path
    let r = callPicking(withReceiver(["cmd": "present", "file": path], name), timeout: 120)
    fail(r)
    let url = r["url"] as? String ?? ""
    print("presenter view: \(url)")
    let open = Process()
    open.executableURL = URL(fileURLWithPath: "/usr/bin/open")
    open.arguments = [url]
    if (try? open.run()) == nil { print("(open it in a browser)") }

case "blank" where (2...3).contains(args.count):
    let (name, word) = args.count == 3 ? (args[1], args[2]) : (nil, args[1])
    guard word == "on" || word == "off" else { print("error: blank on|off, not \(word)"); exit(1) }
    fail(callPicking(withReceiver(["cmd": "annotate", "msg": ["t": "blank", "on": word == "on"]], name)))

case "quiet" where args.count <= 2:
    // Do not disturb while presenting or mirroring; ask: the app asks once.
    let r = call(args.count == 2 ? ["cmd": "quiet", "mode": args[1]] : ["cmd": "quiet"])
    fail(r)
    if args.count == 1 { print(r["mode"] as? String ?? "?") }
    if let note = r["note"] as? String { print(note) }

case "stop-cast":
    fail(callPicking(withReceiver(["cmd": "stop_cast"], args.count > 1 ? args[1] : nil)))

case "browser":
    guard args.count > 1, args[1] == "install" else { print("usage: luchctl browser install"); exit(2) }
    // The running binary itself, not argv[0]: run from PATH that is a bare "luchctl", which would be taken as a file
    // in the current directory and give the browser a host that is not there.
    let exe = (Bundle.main.executableURL ?? URL(fileURLWithPath: CommandLine.arguments[0]))
        .resolvingSymlinksInPath().standardizedFileURL.path
    let support = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support")
    do {
        let done = try NativeHost.install(exe: exe, support: support)
        if done.isEmpty { print("no Chrome, Chromium, Brave, Edge, Yandex, Arc or Firefox found") }
        done.forEach { print($0) }
        print("now add the Luch extension to the browser (browser-extension/ in luch-linux)")
    } catch {
        print("error: \(error.localizedDescription)")
        exit(1)
    }

case "web":
    // The browsers allowed to watch this screen (the web receiver), and the one waiting to be allowed.
    let words = Array(args.dropFirst())
    let r = call(["cmd": "web_clients"])
    fail(r)
    let all = r["clients"] as? [[String: Any]] ?? []
    switch (words.first, words.count) {
    case (nil, _):
        if all.isEmpty {
            let url = call(["cmd": "web_url"])["url"] as? String ?? "https://<this Mac>:47801/"
            print("(no browser allowed yet: open \(url) in a browser and confirm the code here)")
        }
        for (n, b) in all.enumerated() {
            let wait = b["waiting"] as? Bool == true ? " waiting, code \(b["code"] as? String ?? "?")" : ""
            print("\(n + 1). \(b["name"] as? String ?? "?") \(b["address"] as? String ?? "")\(wait)")
        }
    case (let answer?, 2) where answer == "allow" || answer == "deny":
        guard let b = all.first(where: { $0["waiting"] as? Bool == true && $0["code"] as? String == words[1] }) else {
            print("error: no browser waiting with the code \(words[1])")
            exit(1)
        }
        fail(call(["cmd": "approve_web", "request": b["id"] as? String ?? "", "allow": answer == "allow"]))
    case ("forget"?, 2):
        guard let n = Int(words[1]), n >= 1, n <= all.count else {
            print("error: no browser \(words[1]) (see `luchctl web`)")
            exit(1)
        }
        fail(call(["cmd": "forget_web", "id": all[n - 1]["id"] as? String ?? ""]))
    default:
        print("usage: luchctl web [allow|deny <code> | forget <n>]")
        exit(2)
    }

case "pair":
    let words = Array(args.dropFirst())
    guard words.count <= 2 else { print("usage: luchctl pair [r] [code]"); exit(2) }
    if words.count == 1, isPairCode(words[0]) {
        pairWith(nil, code: words[0])
    } else {
        pairWith(words.first, code: words.count > 1 ? words[1] : nil)
    }

case "paired":
    listPaired()

case "forget":
    guard args.count == 2 else { print("usage: luchctl forget <r>"); exit(2) }
    forget(args[1])

case "reports":
    listReports()

case "report":
    guard args.count == 2 || args.count == 3 else { print("usage: luchctl report <n> [open|dismiss]"); exit(2) }
    report(args[1], args.count == 3 ? args[2] : nil)

case "ping":
    let response = call(["cmd": "ping"])
    print((response["ok"] as? Bool) == true ? "app is running" : "no response")

default:
    print("""
    usage: luchctl list | mirror [r] [fast|monitor|window|virtual|audio|asis|extend|speed|quality|mac] [display|window]
                          [--codec auto|h264|hevc] [--profile baseline|main|high] [--frame-ms N]
                   | stats | state | stop | ping
                   | cast <file|link>... [--to r] [--title T] [--start m:ss] [--sub file] [--relay] | stop-cast [r]
                   | media [r] [play|pause|toggle|seek m:ss|skip s|rate x|audio id|sub id|sub off]
                   | volume [r] [0..100|+|-|mute|unmute] | page [r] next|prev|<n> | blank [r] on|off
                   | present [r] <file> | quiet [ask|on|off] | sleep [r] | browser install
                   | find [auto|all|mdns,scan,ble,sound,qr] | add <link|host[:port]> | refresh | id | info [r]
                   | known | wake [r] | wake <r> learn | wake <r> mac|ble <value>|off
                   | pair [r] [code] | paired | forget <r> | reports | report <n> [open|dismiss]
                   | report [--keep-names] [--send] [what happened…]
                   | web [allow|deny <code> | forget <n>]
           present: shows a PDF or a presentation on the receiver and opens the presenter view here (current and
           next slide, notes, timer; the mouse is a laser pointer, dragging draws, a clicker turns the slides)
           quiet: hide notification banners while presenting or mirroring (ask: the app asks once); macOS allows it
           only through Shortcuts, so the app adds two shortcuts the first time
           pair: the first connection to a receiver shows a code on the TV, typed here (any command that connects
           asks for it too); both then know each other and later connections need nothing. forget: pair again next time
           report: this Mac's log and its paired receivers' logs in one zip in Downloads, for a chat or an issue;
           addresses and names hidden unless --keep-names; --send also sends it to the developers (no account;
           when the app was built with a report address) and says its number
    """ + "\n" + togetherUsage + "\n" + calibrateUsage)
    exit(2)
}
