// Just enough XML for UPnP: device descriptions and SOAP answers are small, flat and machine-written, so elements
// are found by their local name (any namespace prefix) instead of building a tree. luchd's luchd/src/tv/xml.rs.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation

public enum XML {
    /// The inner text of every `<name>` element (any prefix), in order. Self-closing ones give "".
    public static func all(_ xml: String, _ name: String) -> [Substring] {
        var out: [Substring] = []
        var at = xml.startIndex
        while let (openEnd, selfClosing, prefix) = findOpen(xml, name, from: at) {
            if selfClosing {
                out.append(xml[openEnd..<openEnd])
                at = openEnd
                continue
            }
            let close = prefix.map { "</\($0):\(name)>" } ?? "</\(name)>"
            guard let end = xml.range(of: close, range: openEnd..<xml.endIndex) else { break }
            out.append(xml[openEnd..<end.lowerBound])
            at = end.upperBound
        }
        return out
    }

    /// The first `<name>`'s text, unescaped and trimmed.
    public static func text(_ xml: String, _ name: String) -> String? {
        all(xml, name).first.map { unescape($0.trimmingCharacters(in: .whitespacesAndNewlines)) }
    }

    /// The value of attribute `attr` on the first `<name ...>` tag.
    public static func attr(_ xml: String, _ name: String, _ attr: String) -> String? {
        guard let (end, _, _) = findOpen(xml, name, from: xml.startIndex),
              let lt = xml[..<end].lastIndex(of: "<") else { return nil }
        let tag = xml[lt..<end]
        guard let key = tag.range(of: " \(attr)=") else { return nil }
        let rest = tag[key.upperBound...]
        guard let q = rest.first, q == "\"" || q == "'" else { return nil }
        let value = rest.dropFirst()
        guard let close = value.firstIndex(of: q) else { return nil }
        return unescape(String(value[..<close]))
    }

    /// The next opening tag `<name` or `<prefix:name` from `from`: where its `>` ends, whether it was `/>`, its prefix.
    private static func findOpen(_ xml: String, _ name: String, from: String.Index) -> (String.Index, Bool, Substring?)? {
        var at = from
        while let lt = xml[at...].firstIndex(of: "<") {
            let rest = xml[xml.index(after: lt)...]
            guard let tagEnd = rest.firstIndex(where: { $0.isWhitespace || $0 == ">" || $0 == "/" }) else { return nil }
            let qname = rest[..<tagEnd]
            let (prefix, local): (Substring?, Substring)
            if let colon = qname.firstIndex(of: ":") {
                (prefix, local) = (qname[..<colon], qname[qname.index(after: colon)...])
            } else {
                (prefix, local) = (nil, qname)
            }
            if local == name, let c = qname.first, !"/?!".contains(c) {
                guard let gt = rest.firstIndex(of: ">") else { return nil }
                let selfClosing = gt > rest.startIndex && xml[xml.index(before: gt)] == "/"
                return (xml.index(after: gt), selfClosing, prefix)
            }
            at = xml.index(after: lt)
        }
        return nil
    }

    public static func escape(_ s: String) -> String {
        var out = ""
        out.reserveCapacity(s.count)
        for c in s {
            switch c {
            case "&": out += "&amp;"
            case "<": out += "&lt;"
            case ">": out += "&gt;"
            case "\"": out += "&quot;"
            case "'": out += "&apos;"
            default: out.append(c)
            }
        }
        return out
    }

    public static func unescape(_ s: String) -> String {
        guard s.contains("&") else { return s }
        var out = ""
        var rest = Substring(s)
        while let amp = rest.firstIndex(of: "&") {
            out += rest[..<amp]
            rest = rest[amp...]
            guard let semi = rest.firstIndex(of: ";"), rest.distance(from: rest.startIndex, to: semi) <= 10 else {
                out += "&"
                rest = rest.dropFirst()
                continue
            }
            let ent = rest[rest.index(after: rest.startIndex)..<semi]
            var c: Character?
            switch ent {
            case "amp": c = "&"
            case "lt": c = "<"
            case "gt": c = ">"
            case "quot": c = "\""
            case "apos": c = "'"
            default:
                let code: UInt32? = ent.hasPrefix("#x") ? UInt32(ent.dropFirst(2), radix: 16)
                    : ent.hasPrefix("#") ? UInt32(ent.dropFirst()) : nil
                c = code.flatMap(Unicode.Scalar.init).map(Character.init)
            }
            if let c {
                out.append(c)
                rest = rest[rest.index(after: semi)...]
            } else {
                out += "&"
                rest = rest.dropFirst()
            }
        }
        out += rest
        return out
    }
}
