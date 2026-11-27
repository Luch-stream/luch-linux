// The card: what a receiver says about itself where there is no room for JSON (PROTOCOL.md 1c, 1d, 1f), and the QR
// link (1e). Bluetooth LE and sound carry the same bytes; luchd's find/card.rs is the reference.
//
//   0..2   "Lu"            magic
//   2      v << 4 | kind   protocol version, 1 = a receiver here, 2 = a sender looking for one
//   3..7   id              the first 4 bytes of the id (its first 8 hex digits)
//   7..11  IPv4            receiver only
//   11..13 port            receiver only, big-endian
//   13..17 token           a one-time pairing token: sound only, never over Bluetooth
//   13..15 PSM             Bluetooth only: the L2CAP channel for connecting directly (Direct.swift), big-endian,
//                          0 = none; with one, the address may be 0.0.0.0:0 (no usable LAN)
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchNet
import LuchProto

public struct Card: Equatable {
    public static let magic: [UInt8] = [0x4c, 0x75]
    public static let receiver: UInt8 = 1
    public static let sender: UInt8 = 2

    public var kind: UInt8
    public var version: UInt8
    /// 8 lowercase hex digits: the start of the id, enough to tell receivers apart and to check the welcome.
    public var id: String
    /// Host-order IPv4 and port.
    public var addr: (ip: UInt32, port: UInt16)?
    /// 8 lowercase hex digits.
    public var token: String?
    /// The L2CAP PSM a receiver takes a direct connection on (Bluetooth only), 0 when it offers none.
    public var psm: UInt16

    public init(kind: UInt8, version: UInt8 = UInt8(Proto.version), id: String, addr: (ip: UInt32, port: UInt16)?,
                token: String? = nil, psm: UInt16 = 0) {
        self.kind = kind
        self.version = version
        self.id = id
        self.addr = addr
        self.token = token
        self.psm = psm
    }

    public static func == (a: Card, b: Card) -> Bool {
        a.kind == b.kind && a.version == b.version && a.id == b.id && a.token == b.token && a.psm == b.psm
            && a.addr?.ip == b.addr?.ip && a.addr?.port == b.addr?.port
    }

    /// A sender looking for receivers: only who is looking.
    public static func looking(_ senderID: String) -> Card {
        Card(kind: sender, id: String(senderID.filter { $0 != "-" }.prefix(8)).lowercased(), addr: nil)
    }

    public func encode() -> [UInt8] {
        var b = Card.magic
        b.append(version << 4 | kind)
        b += hex4(id) ?? [0, 0, 0, 0]
        if addr != nil || psm != 0 {
            let (ip, port) = addr ?? (0, 0)
            b += [UInt8(ip >> 24), UInt8(ip >> 16 & 255), UInt8(ip >> 8 & 255), UInt8(ip & 255)]
            b += [UInt8(port >> 8), UInt8(port & 255)]
            if psm != 0 {
                b += [UInt8(psm >> 8), UInt8(psm & 255)]
            } else if let t = token.flatMap(hex4) {
                b += t
            }
        }
        return b
    }

    /// Nil for anything that is not a card of a version this sender speaks (a card heard by sound).
    public static func decode(_ b: [UInt8]) -> Card? { decode(b, bluetooth: false) }

    /// `bluetooth`: the card of an advertisement, where bytes 13..15 are the PSM instead of a token.
    public static func decode(_ b: [UInt8], bluetooth: Bool) -> Card? {
        guard b.count >= 7, Array(b[0..<2]) == magic else { return nil }
        let (version, kind) = (b[2] >> 4, b[2] & 15)
        guard UInt32(version) == Proto.version, kind == receiver || kind == sender else { return nil }
        let id = hexString(b[3..<7])
        var addr: (ip: UInt32, port: UInt16)?
        if b.count >= 13 {
            addr = (UInt32(b[7]) << 24 | UInt32(b[8]) << 16 | UInt32(b[9]) << 8 | UInt32(b[10]),
                    UInt16(b[11]) << 8 | UInt16(b[12]))
        }
        let psm = bluetooth && b.count >= 15 ? UInt16(b[13]) << 8 | UInt16(b[14]) : 0
        if addr.map({ $0.ip == 0 || $0.port == 0 }) ?? true { addr = nil }
        // A receiver is reached at its address, or directly by its PSM; with neither it is of no use.
        if kind == receiver, addr == nil, psm == 0 { return nil }
        let token = !bluetooth && b.count >= 17 ? hexString(b[13..<17]) : nil
        return Card(kind: kind, version: version, id: id, addr: addr, token: token, psm: psm)
    }

    /// `luch://connect?host=<ip>&port=<port>&id=<8 hex>&t=<token>` (what the receiver's QR code says), or a plain
    /// `host[:port]` typed by a person. Unknown parameters are ignored.
    public static func parseLink(_ s: String) -> Card? {
        let s = s.trimmingCharacters(in: .whitespacesAndNewlines)
        var host: UInt32?
        var port = Proto.controlPort
        var id = ""
        var token: String?
        let prefix = "luch://connect?"
        if s.hasPrefix(prefix) {
            for kv in s.dropFirst(prefix.count).split(separator: "&") {
                let pair = kv.split(separator: "=", maxSplits: 1).map(String.init)
                let (k, v) = (pair[0], pair.count > 1 ? pair[1] : "")
                switch k {
                case "host": host = parseIPv4(v)
                case "port":
                    guard let p = UInt16(v) else { return nil }
                    port = p
                case "id": id = v.lowercased()
                case "t": token = hex4(v) != nil ? v.lowercased() : nil
                default: break
                }
            }
        } else if let colon = s.lastIndex(of: ":") {
            guard let p = UInt16(s[s.index(after: colon)...]) else { return nil }
            host = parseIPv4(String(s[..<colon]))
            port = p
        } else {
            host = parseIPv4(s)
        }
        guard let h = host, h != 0, port != 0 else { return nil }
        return Card(kind: receiver, id: id, addr: (h, port), token: token)
    }
}

/// The first 8 hex digits of `s` (dashes left out) as 4 bytes.
func hex4(_ s: String) -> [UInt8]? {
    let digits = Array(s.filter { $0 != "-" }.prefix(8))
    guard digits.count == 8 else { return nil }
    var out: [UInt8] = []
    for i in stride(from: 0, to: 8, by: 2) {
        guard let b = UInt8(String(digits[i...i + 1]), radix: 16) else { return nil }
        out.append(b)
    }
    return out
}

public func hexString<S: Sequence>(_ b: S) -> String where S.Element == UInt8 {
    b.map { String(format: "%02x", $0) }.joined()
}

/// Bytes of a hex string; spaces, colons and a leading 0x are allowed. Nil for odd lengths or other characters.
public func parseHex(_ s: String) -> [UInt8]? {
    var t = s.filter { !$0.isWhitespace && $0 != ":" }
    if t.hasPrefix("0x") { t.removeFirst(2) }
    guard !t.isEmpty, t.count % 2 == 0 else { return nil }
    let chars = Array(t)
    var out: [UInt8] = []
    for i in stride(from: 0, to: chars.count, by: 2) {
        guard let b = UInt8(String(chars[i...i + 1]), radix: 16) else { return nil }
        out.append(b)
    }
    return out
}
