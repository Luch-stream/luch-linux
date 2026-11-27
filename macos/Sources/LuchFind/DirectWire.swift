// The request for a direct connection (PROTOCOL.md, "direct"), asked over the receiver's TLS control protocol after the
// welcome, on the Bluetooth channel: {"t":"direct"}, answered with the Wi-Fi Direct group's name, passphrase and
// address once the person allowed it on the TV, or with an error that says why not.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchNet

/// The receiver's Wi-Fi Direct group, as it answered.
public struct DirectAnswer: Equatable {
    public var ssid: String
    public var pass: String
    /// MHz, 0 when the receiver does not say.
    public var freq: Int
    /// The receiver's address in the group (the group owner's, 192.168.49.1 on Android).
    public var addr: String
    /// Its plain control port and its TLS port there.
    public var port: UInt16
    public var tlsPort: UInt16

    public init(ssid: String, pass: String, freq: Int = 0, addr: String, port: UInt16, tlsPort: UInt16) {
        self.ssid = ssid
        self.pass = pass
        self.freq = freq
        self.addr = addr
        self.port = port
        self.tlsPort = tlsPort
    }

    /// The answer in `m`, or its refusal in words for the person (`tv` is the receiver's name).
    public static func parse(_ m: [String: Any], tv: String) throws -> DirectAnswer {
        switch m["t"] as? String {
        case "error": throw FindError(refusal(m, tv: tv))
        case "direct": break
        default: throw FindError("\(tv) answered something else than a direct connection")
        }
        func port(_ key: String) -> UInt16? {
            (m[key] as? NSNumber).flatMap { (1...65535).contains($0.intValue) ? UInt16($0.intValue) : nil }
        }
        // WPA2's limits: a passphrase of 8 to 63 characters, an SSID of at most 32 bytes.
        guard let ssid = m["ssid"] as? String, !ssid.isEmpty, ssid.utf8.count <= 32,
              let pass = m["pass"] as? String, (8...63).contains(pass.count),
              let addr = m["addr"] as? String, let ip = parseIPv4(addr), ip != 0,
              let plain = port("port"), let tls = port("tls_port") else {
            throw FindError("\(tv) answered with a Wi-Fi Direct network this Mac cannot join")
        }
        return DirectAnswer(ssid: ssid, pass: pass, freq: (m["freq"] as? NSNumber)?.intValue ?? 0, addr: addr,
                            port: plain, tlsPort: tls)
    }

    /// A receiver's `{"t":"error","for":"direct",...}` in words.
    static func refusal(_ m: [String: Any], tv: String) -> String {
        let detail = (m["detail"] as? String).map { $0.isEmpty ? "" : ": \($0)" } ?? ""
        switch m["code"] as? String ?? "" {
        case "denied": return "\(tv) said no to connecting directly"
        case "no_p2p": return "\(tv) cannot make a Wi-Fi Direct network\(detail)"
        case "not_allowed": return "\(tv) does not allow direct connections\(detail)"
        case "busy": return "\(tv) is busy with another direct connection\(detail); try again in a moment"
        case "timeout": return "nobody answered on \(tv) in time, or its Wi-Fi Direct network did not come up\(detail)"
        case let code: return "\(tv) refused the direct connection (\(code.isEmpty ? "no reason" : code))\(detail)"
        }
    }

    /// Asks on a channel just welcomed. `offered`: the welcome's modes list "direct"; a receiver that does not is
    /// given a few seconds only, as an older one never answers.
    public static func ask(_ ctl: ControlChannel, tv: String, offered: Bool, timeout: TimeInterval) throws -> DirectAnswer {
        guard ctl.send(["t": "direct"]) else { throw FindError("\(tv) closed the connection") }
        let deadline = Date().addingTimeInterval(offered ? timeout : 5)
        while true {
            guard let m = ctl.waitForAny(["direct", "error"], timeout: deadline.timeIntervalSinceNow) else {
                if ctl.isClosed { throw FindError("\(tv) closed the connection") }
                throw FindError(offered ? "nobody answered on \(tv) in time"
                                        : "\(tv) cannot connect directly (no Wi-Fi Direct there, or an older Luch)")
            }
            // Another request's error is not this one's answer.
            if m["t"] as? String == "error", let what = m["for"] as? String, what != "direct" { continue }
            return try parse(m, tv: tv)
        }
    }
}
