// Sealed media (PROTOCOL.md 4, v1): AES-128-GCM over each whole frame, the key from `ready` over TLS. One seal per
// frame, not per datagram: tens of microseconds for a 1080p frame, and nothing waits on it.
// SPDX-License-Identifier: GPL-3.0-or-later
import CryptoKit
import Foundation

public struct MediaSeal {
    /// The GCM tag after each sealed frame; a sealed frame's `frame length` counts it.
    public static let tag = 16
    private let key: SymmetricKey

    public init?(key: [UInt8]) {
        guard key.count == 16 else { return nil }
        self.key = SymmetricKey(data: key)
    }

    /// The frame as it goes on the wire, ciphertext then tag. `header` is the frame's 24-byte header (any shard index),
    /// with `frame length` already counting the tag: all of it but the index and the parity bit is authenticated.
    public func seal(header: [UInt8], frame: [UInt8]) -> [UInt8] {
        var aad = Array(header.prefix(Proto.headerSize))
        aad[1] &= ~PacketFlags.parity.rawValue
        aad[2] = 0
        aad[3] = 0
        // kind | 0 0 0 | seq | 0 0 0 0: never the same twice under one session's key.
        var nonce = [UInt8](repeating: 0, count: 12)
        nonce[0] = header[0]
        nonce[4..<8] = header[8..<12]
        // A 12-byte nonce and a frame far below GCM's limit: this cannot fail.
        let box = try! AES.GCM.seal(frame, using: key, nonce: try! AES.GCM.Nonce(data: nonce), authenticating: aad)
        var out = [UInt8]()
        out.reserveCapacity(frame.count + Self.tag)
        out.append(contentsOf: box.ciphertext)
        out.append(contentsOf: box.tag)
        return out
    }

    /// The media key a receiver gives in `ready` over TLS; nil from an older receiver (plain media).
    public static func key(ready: [String: Any]) throws -> [UInt8]? {
        guard let crypt = ready["crypt"] as? String else { return nil }
        guard crypt == "aes-128-gcm" else {
            throw MediaSealError("the receiver wants media sealed with \(crypt), unknown here")
        }
        guard let b64 = ready["key"] as? String, let k = Data(base64Encoded: b64), k.count == 16 else {
            throw MediaSealError("bad media key in ready")
        }
        return [UInt8](k)
    }
}

public struct MediaSealError: Error, LocalizedError {
    public let errorDescription: String?
    init(_ why: String) { errorDescription = why }
}
