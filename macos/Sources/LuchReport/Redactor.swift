// What "Hide addresses and names" does to every text of a problem report: IPv4 addresses become 192.0.2.N and IPv6
// ones 2001:db8::N (the ranges kept for examples, so the family still shows: a VPN, a link-local route), MAC
// addresses 02:00:00:00:00:NN, and the names given to it (this computer's, its user's, the receivers') "Computer",
// "User", "Receiver 1"… Each one stays the same through the whole report, so lines still tell who is who.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation

public final class Redactor {
    private var v4: [String: String] = [:]
    private var v6: [String: String] = [:]
    private var macs: [String: String] = [:]
    /// Lowercased name -> what stands for it.
    private var names: [String: String] = [:]
    private var receivers = 0

    public init() {}

    /// A name to hide as `placeholder` (e.g. "Computer", "User"); names shorter than 2 letters are left alone.
    public func hide(_ name: String, as placeholder: String) {
        let n = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard n.count >= 2, names[n.lowercased()] == nil else { return }
        names[n.lowercased()] = placeholder
    }

    /// A receiver's names (from discovery, its welcome): all of them become the same "Receiver N", which is returned.
    @discardableResult
    public func receiver(_ aliases: [String]) -> String {
        if let known = aliases.lazy.compactMap({ self.names[$0.lowercased()] }).first {
            aliases.forEach { hide($0, as: known) }
            return known
        }
        receivers += 1
        let label = "Receiver \(receivers)"
        aliases.forEach { hide($0, as: label) }
        return label
    }

    /// What stands for `name`, if it is hidden.
    public func label(_ name: String) -> String? { names[name.lowercased()] }

    public func redact(_ text: String) -> String {
        var t = text
        // Names first (a name may hold an address-looking part), the longest first so a short one inside it waits.
        for (name, placeholder) in names.sorted(by: { $0.key.count > $1.key.count }) {
            t = replace(Self.nameRegex(name), in: t) { _ in placeholder }
        }
        t = replace(Self.mac, in: t) { m in self.mac(m) }
        t = replace(Self.ipv6, in: t) { m in self.ipv6(m) }
        t = replace(Self.ipv4, in: t) { m in self.ipv4(m) }
        return t
    }

    // MARK: - addresses

    private static let ipv4 = try! NSRegularExpression(pattern: #"(?<![0-9.])(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?![0-9]|\.[0-9])"#)
    /// Hex groups and colons (an IPv4 tail allowed) standing alone; inet_pton then says whether it is an address.
    private static let ipv6 = try! NSRegularExpression(pattern: #"(?<![\w:.])[0-9A-Fa-f]{0,4}(?::[0-9A-Fa-f]{0,4}){2,7}(?:\.[0-9]{1,3}){0,3}(?![\w:])"#)
    private static let mac = try! NSRegularExpression(pattern: #"(?<![\w:-])[0-9A-Fa-f]{2}([:-])[0-9A-Fa-f]{2}(?:\1[0-9A-Fa-f]{2}){4}(?![\w:-])"#)

    private func ipv4(_ s: String) -> String {
        var a = in_addr()
        guard inet_pton(AF_INET, s, &a) == 1 else { return s }
        let first = s.split(separator: ".").first.flatMap { Int($0) } ?? 0
        // Loopback, "any", broadcast and multicast groups name nobody and help reading.
        if first == 127 || first >= 224 || s == "0.0.0.0" { return s }
        if let known = v4[s] { return known }
        let p = "192.0.2.\(v4.count + 1)"
        v4[s] = p
        return p
    }

    private func ipv6(_ s: String) -> String {
        var a = in6_addr()
        guard inet_pton(AF_INET6, s, &a) == 1 else { return s }
        let lower = s.lowercased()
        if lower == "::" || lower == "::1" || lower.hasPrefix("ff") { return s }
        // The same address however it is written.
        var buf = [CChar](repeating: 0, count: Int(INET6_ADDRSTRLEN))
        inet_ntop(AF_INET6, &a, &buf, socklen_t(INET6_ADDRSTRLEN))
        let key = String(cString: buf)
        if let known = v6[key] { return known }
        let p = "2001:db8::\(v6.count + 1)"
        v6[key] = p
        return p
    }

    private func mac(_ s: String) -> String {
        let key = s.lowercased().replacingOccurrences(of: "-", with: ":")
        if key == "00:00:00:00:00:00" || key == "ff:ff:ff:ff:ff:ff" { return s }
        if let known = macs[key] { return known }
        let p = String(format: "02:00:00:00:00:%02x", (macs.count + 1) & 0xff)
        macs[key] = p
        return p
    }

    // MARK: - text

    /// The name as a whole word, in any case; a name ending in a letter does not eat the start of a longer word.
    private static func nameRegex(_ name: String) -> NSRegularExpression {
        let escaped = NSRegularExpression.escapedPattern(for: name)
        let start = name.first.map { $0.isLetter || $0.isNumber } == true ? #"(?<![\p{L}\p{N}])"# : ""
        let end = name.last.map { $0.isLetter || $0.isNumber } == true ? #"(?![\p{L}\p{N}])"# : ""
        return try! NSRegularExpression(pattern: start + escaped + end, options: [.caseInsensitive])
    }

    private func replace(_ re: NSRegularExpression, in text: String, with f: (String) -> String) -> String {
        let ns = text as NSString
        var out = ""
        var last = 0
        for m in re.matches(in: text, range: NSRange(location: 0, length: ns.length)) {
            out += ns.substring(with: NSRange(location: last, length: m.range.location - last))
            out += f(ns.substring(with: m.range))
            last = m.range.location + m.range.length
        }
        return out + ns.substring(from: last)
    }
}
