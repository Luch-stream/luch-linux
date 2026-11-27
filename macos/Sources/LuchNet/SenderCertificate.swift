// The certificate this Mac shows every receiver over TLS (PROTOCOL.md 3a): made once, kept next to the sender's id,
// the key readable only by its user. Its fingerprint is what a receiver pins. No keychain: nothing for macOS to ask
// about, and the files are the same as luchd's (sender-cert.pem, sender-key.pem).
// SPDX-License-Identifier: GPL-3.0-or-later
import CryptoKit
import Darwin
import Foundation
import Security

/// ~/Library/Application Support/Luch: the sender's id, certificate, pins and reports.
public func supportDirectory() -> URL {
    let dir = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Application Support/Luch", isDirectory: true)
    try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
}

public typealias Fingerprint = [UInt8]

/// SHA-256 of a certificate's DER: all either side checks of the other.
public func fingerprint(_ der: [UInt8]) -> Fingerprint {
    Array(SHA256.hash(data: der))
}

public func hex(_ b: [UInt8]) -> String {
    b.map { String(format: "%02x", $0) }.joined()
}

public func unhex(_ s: String) -> [UInt8]? {
    let c = Array(s.utf8)
    guard c.count % 2 == 0 else { return nil }
    var out = [UInt8]()
    out.reserveCapacity(c.count / 2)
    for i in stride(from: 0, to: c.count, by: 2) {
        guard let b = UInt8(String(decoding: c[i..<i + 2], as: UTF8.self), radix: 16) else { return nil }
        out.append(b)
    }
    return out
}

public struct CertificateError: Error, LocalizedError {
    public let errorDescription: String?
    init(_ why: String) { errorDescription = why }
}

public final class SenderCertificate {
    public let der: [UInt8]
    public let fp: Fingerprint
    /// For Network.framework's TLS: the certificate with its key, in this process only.
    public let identity: SecIdentity

    private static let lock = NSLock()
    private static var loaded: SenderCertificate?

    /// The one in ~/Library/Application Support/Luch, made there the first time.
    public static func shared() throws -> SenderCertificate {
        lock.lock()
        defer { lock.unlock() }
        if let loaded { return loaded }
        let c = try load(dir: supportDirectory())
        loaded = c
        return c
    }

    public static func load(dir: URL) throws -> SenderCertificate {
        let certURL = dir.appendingPathComponent("sender-cert.pem")
        let keyURL = dir.appendingPathComponent("sender-key.pem")
        let fm = FileManager.default
        if !fm.fileExists(atPath: certURL.path) || !fm.fileExists(atPath: keyURL.path) {
            let key = P256.Signing.PrivateKey()
            let der = try selfSigned(key)
            try? fm.removeItem(at: keyURL)
            try writePrivate(keyURL, Data(key.pemRepresentation.utf8))
            try Data(pem("CERTIFICATE", der).utf8).write(to: certURL, options: .atomic)
        }
        guard let certText = try? String(contentsOf: certURL, encoding: .utf8), let der = unpem(certText) else {
            throw CertificateError("\(certURL.path) is not a certificate")
        }
        let key: P256.Signing.PrivateKey
        do {
            key = try P256.Signing.PrivateKey(pemRepresentation: String(contentsOf: keyURL, encoding: .utf8))
        } catch {
            throw CertificateError("\(keyURL.path): \(error.localizedDescription)")
        }
        return SenderCertificate(der: der, identity: try secIdentity(der: der, key: key))
    }

    private init(der: [UInt8], identity: SecIdentity) {
        self.der = der
        self.fp = fingerprint(der)
        self.identity = identity
    }

    /// The key file, created readable by its owner only (never world-readable, not even for a moment).
    private static func writePrivate(_ url: URL, _ data: Data) throws {
        let fd = Darwin.open(url.path, O_WRONLY | O_CREAT | O_EXCL, 0o600)
        guard fd >= 0 else { throw CertificateError("\(url.path): \(String(cString: strerror(errno)))") }
        defer { close(fd) }
        let ok = data.withUnsafeBytes { Darwin.write(fd, $0.baseAddress, $0.count) == $0.count }
        if !ok { throw CertificateError("\(url.path): \(String(cString: strerror(errno)))") }
    }

    // MARK: - the certificate

    /// A self-signed X.509 v3 certificate, CN "Luch sender", ECDSA P-256 with SHA-256, as luchd's rcgen makes it:
    /// pinned, never checked for dates, so valid until 2055 so that no TLS library minds.
    static func selfSigned(_ key: P256.Signing.PrivateKey) throws -> [UInt8] {
        let ecdsaSHA256 = DER.seq(DER.oid("1.2.840.10045.4.3.2"))
        let name = DER.seq(DER.tlv(0x31, DER.seq(DER.oid("2.5.4.3"), DER.utf8("Luch sender"))))
        var serial = [UInt8](repeating: 0, count: 16)
        guard SecRandomCopyBytes(kSecRandomDefault, serial.count, &serial) == errSecSuccess else {
            throw CertificateError("no randomness")
        }
        serial[0] &= 0x7f
        let spki = DER.seq(DER.seq(DER.oid("1.2.840.10045.2.1"), DER.oid("1.2.840.10045.3.1.7")),
                           DER.bits(Array(key.publicKey.x963Representation)))
        let tbs = DER.seq(DER.tlv(0xa0, DER.int([2])), DER.int(serial), ecdsaSHA256, name,
                          DER.seq(DER.time("750101000000Z"), DER.time("20550101000000Z")), name, spki)
        let signature = try key.signature(for: Data(tbs)).derRepresentation
        return DER.seq(tbs, ecdsaSHA256, DER.bits(Array(signature)))
    }

    /// The certificate and its key as one SecIdentity, without the keychain. macOS 14 has no public way to that
    /// (PKCS#12 import to memory only came with macOS 15), but Security has had SecIdentityCreate all along.
    private static func secIdentity(der: [UInt8], key: P256.Signing.PrivateKey) throws -> SecIdentity {
        guard let cert = SecCertificateCreateWithData(nil, Data(der) as CFData) else {
            throw CertificateError("the sender certificate does not parse")
        }
        var err: Unmanaged<CFError>?
        let attrs: [CFString: Any] = [kSecAttrKeyType: kSecAttrKeyTypeECSECPrimeRandom,
                                      kSecAttrKeyClass: kSecAttrKeyClassPrivate, kSecAttrKeySizeInBits: 256]
        guard let secKey = SecKeyCreateWithData(key.x963Representation as CFData, attrs as CFDictionary, &err) else {
            throw CertificateError("the sender key: \(err?.takeRetainedValue().localizedDescription ?? "?")")
        }
        typealias Create = @convention(c) (CFAllocator?, SecCertificate, SecKey) -> Unmanaged<SecIdentity>?
        guard let sym = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "SecIdentityCreate"),
              let identity = unsafeBitCast(sym, to: Create.self)(nil, cert, secKey)?.takeRetainedValue() else {
            throw CertificateError("this macOS makes no TLS identity of the sender certificate")
        }
        return identity
    }

    static func pem(_ label: String, _ der: [UInt8]) -> String {
        let b64 = Data(der).base64EncodedString(options: [.lineLength64Characters, .endLineWithLineFeed])
        return "-----BEGIN \(label)-----\n\(b64)\n-----END \(label)-----\n"
    }

    static func unpem(_ text: String) -> [UInt8]? {
        let body = text.split(separator: "\n").filter { !$0.hasPrefix("-----") }.joined()
        return Data(base64Encoded: body).map { [UInt8]($0) }
    }
}

/// Just enough DER for one certificate.
enum DER {
    static func tlv(_ tag: UInt8, _ body: [UInt8]) -> [UInt8] {
        var len: [UInt8] = []
        if body.count < 0x80 {
            len = [UInt8(body.count)]
        } else {
            var n = body.count
            while n > 0 { len.insert(UInt8(n & 0xff), at: 0); n >>= 8 }
            len.insert(0x80 | UInt8(len.count), at: 0)
        }
        return [tag] + len + body
    }

    static func seq(_ parts: [UInt8]...) -> [UInt8] { tlv(0x30, parts.flatMap { $0 }) }

    static func oid(_ dotted: String) -> [UInt8] {
        let arcs = dotted.split(separator: ".").compactMap { UInt64($0) }
        var out: [UInt8] = [UInt8(arcs[0] * 40 + arcs[1])]
        for arc in arcs.dropFirst(2) {
            var chunk: [UInt8] = [UInt8(arc & 0x7f)]
            var rest = arc >> 7
            while rest > 0 { chunk.insert(UInt8(rest & 0x7f) | 0x80, at: 0); rest >>= 7 }
            out += chunk
        }
        return tlv(0x06, out)
    }

    /// A non-negative INTEGER: no extra leading zeros, one when the top bit is set.
    static func int(_ bytes: [UInt8]) -> [UInt8] {
        var b = bytes
        while b.count > 1 && b[0] == 0 && b[1] & 0x80 == 0 { b.removeFirst() }
        if b[0] & 0x80 != 0 { b.insert(0, at: 0) }
        return tlv(0x02, b)
    }

    static func bits(_ b: [UInt8]) -> [UInt8] { tlv(0x03, [0] + b) }
    static func utf8(_ s: String) -> [UInt8] { tlv(0x0c, Array(s.utf8)) }
    /// UTCTime (YYMMDDhhmmssZ) or, from 2050, GeneralizedTime (YYYYMMDDhhmmssZ).
    static func time(_ s: String) -> [UInt8] { tlv(s.count == 13 ? 0x17 : 0x18, Array(s.utf8)) }
}
