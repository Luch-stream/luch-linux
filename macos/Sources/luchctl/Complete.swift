// Tab completion: `luchctl __complete <words…> <current>` prints what may stand at the current word, one per line as
// "value<TAB>what it does", and a first line ":files" when a file may stand there too. The shells' scripts
// (`luchctl completion zsh|bash|fish`) only pass the words and show the answer, so the commands are known in one place.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchLocal

enum Complete {
    struct Out {
        var items: [(String, String)] = []
        var files = false

        mutating func add(_ list: [(String, String)]) { items += list }
        mutating func add(_ words: String...) { items += words.map { ($0, "") } }
    }

    static let commands: [(String, String)] = [
        ("list", "receivers on this network"), ("mirror", "show this Mac's screen on a receiver"),
        ("stop", "stop showing the screen"), ("stats", "how the session runs"), ("state", "what the app does now"),
        ("cast", "show a file or a link on a receiver"), ("stop-cast", "stop what a receiver plays"),
        ("media", "play, pause, seek what a receiver plays"), ("volume", "a receiver's volume"),
        ("page", "turn a document's pages"), ("blank", "black out a receiver's screen"),
        ("present", "a presentation with the presenter view here"), ("quiet", "hide notifications while showing"),
        ("watch-device", "watch the TV's heat and battery"),
        ("sleep", "put a receiver to sleep"), ("wake", "turn a receiver on"), ("find", "look harder for receivers"),
        ("add", "a receiver by its QR link or address"), ("refresh", "look for receivers again"),
        ("info", "what a receiver is and can do"), ("id", "this Mac's id"), ("known", "receivers met before"),
        ("pair", "pair with a receiver (its code)"), ("paired", "receivers this Mac is paired with"),
        ("forget", "forget a pairing"), ("reports", "receivers' crash reports"), ("report", "a problem report, or one crash report"),
        ("web", "browsers allowed to watch"), ("audio", "a receiver's sound mode"),
        ("latency", "the latency lab"), ("group", "several receivers in step"),
        ("offset", "a receiver's own sound correction"), ("calibrate", "time a group's receivers by microphone"),
        ("direct", "connect to a receiver over its own Wi-Fi"), ("browser", "connect the browser extension"),
        ("ping", "is the app running"), ("completion", "tab completion for a shell"),
    ]

    static func run(_ words: [String]) {
        let out = answer(words, receivers: receivers)
        var lines: [String] = out.files ? [":files"] : []
        lines += out.items.map { $0.1.isEmpty ? $0.0 : "\($0.0)\t\($0.1)" }
        print(lines.joined(separator: "\n"))
        exit(0)
    }

    /// The receivers' names, from the app; none when it does not answer at once.
    static func receivers() -> [(String, String)] {
        let r = (try? LocalClient.request(["cmd": "receivers"], timeout: 1.5))?["receivers"] as? [[String: Any]] ?? []
        return r.compactMap { d in (d["name"] as? String).map { ($0, d["model"] as? String ?? "receiver") } }
    }

    /// `words`: after "luchctl", the last one being the word under the cursor (maybe empty).
    static func answer(_ words: [String], receivers: () -> [(String, String)]) -> Out {
        var out = Out()
        guard words.count > 1 else {
            out.add(commands)
            return out
        }
        let cmd = words[0]
        let before = Array(words.dropFirst().dropLast())
        let current = words.last ?? ""
        let prev = words.count >= 2 ? words[words.count - 2] : ""
        lazy var names = receivers()
        // Whether a receiver is named already among the words before (the optional [r] of most commands).
        func named() -> Bool { before.contains { w in names.contains { $0.0 == w } } }
        func receiverThen(_ list: [(String, String)]) {
            if !named() && before.isEmpty { out.add(names) }
            out.add(list)
        }

        switch cmd {
        case "mirror":
            switch prev {
            case "--codec": out.add("auto", "h264", "hevc")
            case "--profile": out.add("baseline", "main", "high")
            case "--frame-ms": break
            case "monitor": out.add(lines("displays", key: "displays"))
            case "window": out.add(lines("windows", key: "windows"))
            default:
                if !named() && !before.contains(where: { mirrorSources.map(\.0).contains($0) }) { out.add(names) }
                if !before.contains(where: { mirrorSources.map(\.0).contains($0) }) { out.add(mirrorSources) }
                if current.hasPrefix("-") { out.add("--codec", "--profile", "--frame-ms") }
            }
        case "stop", "stop-cast", "sleep", "info", "forget", "pair", "direct":
            if before.isEmpty {
                out.add(names)
                if cmd == "direct" { out.add([("off", "leave the receiver's Wi-Fi, back to this Mac's own")]) }
            }
        case "cast":
            switch prev {
            case "--to": out.add(names)
            case "--sub": out.files = true
            case "--title", "--start": break
            default:
                out.files = true
                if current.hasPrefix("-") {
                    out.add([("--to", "which receiver"), ("--title", "the title shown"), ("--start", "start at m:ss"),
                             ("--sub", "a subtitles file"), ("--relay", "through this Mac"), ("--paused", "start paused")])
                }
            }
        case "present":
            if before.isEmpty { out.add(names) }
            out.files = true
        case "media":
            switch prev {
            case "sub": out.add([("off", "no subtitles")])
            case "seek", "skip", "rate", "audio": break
            default:
                receiverThen([("play", ""), ("pause", ""), ("toggle", "play or pause"), ("seek", "to m:ss"),
                              ("skip", "by seconds, - back"), ("rate", "speed, 1 = normal"), ("audio", "a track"),
                              ("sub", "subtitles"), ("watch", "follow what plays")])
            }
        case "volume": receiverThen([("mute", ""), ("unmute", ""), ("+", "louder"), ("-", "quieter")])
        case "page": receiverThen([("next", ""), ("prev", "")])
        case "blank": receiverThen([("on", "black screen"), ("off", "picture back")])
        case "audio":
            receiverThen([("auto", "held where the sound lags"), ("on", ""), ("off", "picture only"),
                          ("sync", "always held")])
        case "quiet": if before.isEmpty { out.add([("ask", "ask once"), ("on", ""), ("off", "")]) }
        case "watch-device": if before.isEmpty { out.add([("on", "a badge on a hot TV"), ("off", "")]) }
        case "browser": if before.isEmpty { out.add([("install", "Chrome, Firefox and others")]) }
        case "calibrate": if before.isEmpty { out.add([("self", "this Mac's speaker to its microphone")]) }
        case "completion": if before.isEmpty { out.add("zsh", "bash", "fish") }
        case "web":
            if before.isEmpty {
                out.add([("allow", "a browser's code"), ("deny", "a browser's code"), ("forget", "an allowed browser")])
            }
        case "report":
            if before.isEmpty { out.add([("--keep-names", "a problem report with addresses and names")]) }
            else if before.count == 1, Int(before[0]) != nil { out.add([("open", "the issue, filled in"), ("dismiss", "")]) }
        case "wake":
            if before.isEmpty { out.add(names) }
            else if before.count == 1 {
                out.add([("learn", "press the remote's power key with the TV off"), ("mac", "its Wi-Fi address"),
                         ("ble", "its remote's signal"), ("off", "do not wake it")])
            }
        case "latency":
            if before.isEmpty { out.add([("on", "the lab on a receiver"), ("off", ""), ("analyze", "a slow-motion video")]) }
            else if before.first == "analyze" { out.files = true }
            else if before.count == 1 { out.add(names) }
        case "offset": if before.isEmpty { out.add(names) }
        case "group":
            if before.isEmpty {
                out.add([("watch", "all at one delay: music, rooms"), ("interactive", "the picture as fast as alone")])
            } else if prev == "--source" {
                out.add([("fast", "the screen"), ("monitor", "a display"), ("audio", "this Mac's sound only")])
            } else if let eq = current.firstIndex(of: "=") {
                let name = String(current[..<eq])
                out.add(["video", "audio", "both"].map { ("\(name)=\($0)", "") })
            } else {
                out.add(names.filter { n in !before.contains { $0 == n.0 || $0.hasPrefix(n.0 + "=") } })
                if current.hasPrefix("-") { out.add([("--source", "what plays")]) }
            }
        case "find":
            if before.isEmpty {
                // A list after commas: complete the last part.
                let head = current.range(of: ",", options: .backwards).map { String(current[..<$0.upperBound]) } ?? ""
                if head.isEmpty { out.add([("auto", "the usual ways"), ("all", "every way")]) }
                let chosen = Set(head.split(separator: ",").map(String.init))
                out.add(findWays.filter { !chosen.contains($0.0) }.map { (head + $0.0, $0.1) })
            }
        default: break
        }
        return out
    }

    static let mirrorSources: [(String, String)] = [
        ("asis", "the screen as it is"), ("fast", "the screen as it is"), ("speed", "at the TV's mode, fastest"),
        ("quality", "at the TV's mode, sharpest"), ("mac", "at the TV's mode, like this Mac"),
        ("extend", "a second screen"), ("virtual", "a second screen"), ("monitor", "one display"),
        ("window", "one window"), ("audio", "this Mac's sound only"),
    ]

    static let findWays: [(String, String)] = [
        ("mdns", "Bonjour"), ("scan", "every address of the network"), ("ble", "Bluetooth"),
        ("sound", "the TV says its address, the microphone listens"), ("qr", "the TV's QR code, the camera reads"),
        ("direct", "the TV's own Wi-Fi, over Bluetooth"),
    ]

    /// Displays or windows, as the app lists them.
    static func lines(_ cmd: String, key: String) -> [(String, String)] {
        let r = (try? LocalClient.request(["cmd": cmd], timeout: 1.5))?[key] as? [[String: Any]] ?? []
        return r.compactMap { d in
            let name = d["name"] as? String ?? d["title"] as? String
            return name.map { ($0, d["app"] as? String ?? "") }
        }
    }

    // MARK: - the shells

    static func script(_ shell: String) -> String? {
        switch shell {
        case "zsh": return zsh
        case "bash": return bash
        case "fish": return fish
        default: return nil
        }
    }

    static let zsh = #"""
    #compdef luchctl
    # luchctl's tab completion: luchctl itself says what may come next (luchctl __complete).
    _luchctl() {
      local -a items
      local line value desc files=0
      for line in "${(@f)$(luchctl __complete "${(@Q)words[2,CURRENT]}" 2>/dev/null)}"; do
        [[ -z $line ]] && continue
        if [[ $line == ":files" ]]; then files=1; continue; fi
        value=${line%%$'\t'*}
        desc=${line#*$'\t'}
        [[ $desc == $line ]] && desc=""
        items+=("${value//:/\\:}${desc:+:$desc}")
      done
      (( ${#items} )) && _describe -t luchctl 'luchctl' items -o nosort
      (( files )) && _files
      return 0
    }
    if [[ $zsh_eval_context[-1] == loadautofunc ]]; then _luchctl "$@"; else compdef _luchctl luchctl; fi
    """#

    static let bash = #"""
    # luchctl's tab completion: luchctl itself says what may come next (luchctl __complete).
    _luchctl() {
      local cur=${COMP_WORDS[COMP_CWORD]} line files=0
      local -a vals=()
      while IFS= read -r line; do
        [[ -z $line ]] && continue
        if [[ $line == ":files" ]]; then files=1; continue; fi
        vals+=("${line%%$'\t'*}")
      done < <(luchctl __complete "${COMP_WORDS[@]:1:COMP_CWORD}" 2>/dev/null)
      COMPREPLY=()
      local v
      for v in "${vals[@]}"; do [[ $v == "$cur"* ]] && COMPREPLY+=("$(printf '%q' "$v")"); done
      if (( files )); then
        local f
        while IFS= read -r f; do COMPREPLY+=("$f"); done < <(compgen -f -- "$cur")
      fi
    }
    complete -o filenames -F _luchctl luchctl
    """#

    static let fish = #"""
    # luchctl's tab completion: luchctl itself says what may come next (luchctl __complete).
    function __luchctl_complete
        set -l words (commandline -opc)[2..-1] (commandline -ct)
        set -l files 0
        for line in (luchctl __complete $words 2>/dev/null)
            if test "$line" = ":files"
                set files 1
            else
                echo $line
            end
        end
        if test $files = 1
            __fish_complete_path (commandline -ct)
        end
    end
    complete -c luchctl -f -a '(__luchctl_complete)'
    """#
}
