// A self-signed TLS identity made here, without the keychain: a P-256 key and an X.509 v3 certificate written in DER
// by hand (the names it is for in subjectAltName), signed with that key. The web receiver serves its page with one,
// as luchd's web_tls.rs does with rcgen; the tests' fake Chromecast has one, as a real one does.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import Network
import Security

public struct SelfSigned {
    public let key: SecKey
    public let certificate: Data

    /// A new key and a certificate for `names` (DNS names and IPv4 addresses).
    public init(names: [String], commonName: String = "Luch local receiver") throws {
        var error: Unmanaged<CFError>?
        let attrs: [String: Any] = [kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
                                    kSecAttrKeySizeInBits as String: 256]
        guard let key = SecKeyCreateRandomKey(attrs as CFDictionary, &error) else {
            throw TVError("cannot make a key: \(error!.takeRetainedValue())")
        }
        try self.init(key: key, names: names, commonName: commonName)
    }

    /// A certificate for `names` with an existing key.
    public init(key: SecKey, names: [String], commonName: String = "Luch local receiver") throws {
        self.key = key
        certificate = try Self.certificate(key: key, names: names, commonName: commonName)
    }

    /// A certificate made before, with its key.
    public init(key: SecKey, certificate: Data) {
        self.key = key
        self.certificate = certificate
    }

    /// The key as X9.63 bytes (04 || X || Y || D), to keep in a file.
    public var keyData: Data {
        (SecKeyCopyExternalRepresentation(key, nil) as Data?) ?? Data()
    }

    public static func key(from data: Data) -> SecKey? {
        let attrs: [String: Any] = [kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
                                    kSecAttrKeyClass as String: kSecAttrKeyClassPrivate,
                                    kSecAttrKeySizeInBits as String: 256]
        return SecKeyCreateWithData(data as CFData, attrs as CFDictionary, nil)
    }

    /// The identity a TLS server presents.
    public func identity() throws -> SecIdentity {
        guard let cert = SecCertificateCreateWithData(nil, certificate as CFData) else { throw TVError("a bad certificate") }
        guard let id = SecIdentityCreate(nil, cert, key) else { throw TVError("the key does not fit the certificate") }
        return id
    }

    /// TLS server parameters with this identity.
    public func serverParameters() throws -> NWParameters {
        let tls = NWProtocolTLS.Options()
        guard let id = sec_identity_create(try identity()) else { throw TVError("no TLS identity") }
        sec_protocol_options_set_local_identity(tls.securityProtocolOptions, id)
        return NWParameters(tls: tls, tcp: NWProtocolTCP.Options())
    }

    // MARK: - DER

    enum DER {
        static func length(_ n: Int) -> Data {
            if n < 0x80 { return Data([UInt8(n)]) }
            var bytes: [UInt8] = []
            var n = n
            while n > 0 { bytes.insert(UInt8(n & 0xff), at: 0); n >>= 8 }
            return Data([0x80 | UInt8(bytes.count)] + bytes)
        }

        static func tlv(_ tag: UInt8, _ content: Data) -> Data { Data([tag]) + length(content.count) + content }
        static func seq(_ parts: Data...) -> Data { tlv(0x30, parts.reduce(Data(), +)) }
        static func set(_ parts: Data...) -> Data { tlv(0x31, parts.reduce(Data(), +)) }

        static func integer(_ bytes: Data) -> Data {
            var b = bytes.drop { $0 == 0 }
            if b.isEmpty { b = Data([0]) }
            if b.first! & 0x80 != 0 { b.insert(0, at: b.startIndex) }
            return tlv(0x02, Data(b))
        }

        static func oid(_ s: String) -> Data {
            let n = s.split(separator: ".").compactMap { UInt64($0) }
            var out = Data([UInt8(n[0] * 40 + n[1])])
            for var v in n.dropFirst(2) {
                var chunk: [UInt8] = [UInt8(v & 0x7f)]
                v >>= 7
                while v > 0 { chunk.insert(UInt8(v & 0x7f) | 0x80, at: 0); v >>= 7 }
                out.append(contentsOf: chunk)
            }
            return tlv(0x06, out)
        }

        static func utf8(_ s: String) -> Data { tlv(0x0c, Data(s.utf8)) }
        static func bits(_ d: Data) -> Data { tlv(0x03, Data([0]) + d) }
        static func octets(_ d: Data) -> Data { tlv(0x04, d) }

        /// UTCTime before 2050, GeneralizedTime from then on (RFC 5280 4.1.2.5).
        static func time(_ date: Date) -> Data {
            let f = DateFormatter()
            f.locale = Locale(identifier: "en_US_POSIX")
            f.timeZone = TimeZone(identifier: "UTC")
            let year = Calendar(identifier: .gregorian).dateComponents(in: f.timeZone, from: date).year ?? 2000
            f.dateFormat = year < 2050 ? "yyMMddHHmmss'Z'" : "yyyyMMddHHmmss'Z'"
            return tlv(year < 2050 ? 0x17 : 0x18, Data(f.string(from: date).utf8))
        }
    }

    static func certificate(key: SecKey, names: [String], commonName: String) throws -> Data {
        guard let pub = SecKeyCopyPublicKey(key), let point = SecKeyCopyExternalRepresentation(pub, nil) as Data? else {
            throw TVError("no public key")
        }
        let ecdsaSHA256 = DER.seq(DER.oid("1.2.840.10045.4.3.2"))
        let name = DER.seq(DER.set(DER.seq(DER.oid("2.5.4.3"), DER.utf8(commonName))))
        let spki = DER.seq(DER.seq(DER.oid("1.2.840.10045.2.1"), DER.oid("1.2.840.10045.3.1.7")), DER.bits(point))
        var altNames = Data()
        for n in names {
            var a = in_addr()
            if inet_pton(AF_INET, n, &a) == 1 {
                altNames += DER.tlv(0x87, withUnsafeBytes(of: &a) { Data($0) }) // iPAddress
            } else {
                altNames += DER.tlv(0x82, Data(n.utf8)) // dNSName
            }
        }
        let extensions = DER.tlv(0xa3, DER.seq(
            DER.seq(DER.oid("2.5.29.17"), DER.octets(DER.tlv(0x30, altNames))),
            DER.seq(DER.oid("2.5.29.37"), DER.octets(DER.seq(DER.oid("1.3.6.1.5.5.7.3.1"))))
        ))
        var serial = Data(count: 16)
        _ = serial.withUnsafeMutableBytes { SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!) }
        serial[0] &= 0x7f
        let now = Date()
        let tbs = DER.seq(
            DER.tlv(0xa0, DER.integer(Data([2]))),
            DER.integer(serial),
            ecdsaSHA256,
            name,
            DER.seq(DER.time(now.addingTimeInterval(-86400)), DER.time(now.addingTimeInterval(10 * 365 * 86400))),
            name,
            spki,
            extensions
        )
        var error: Unmanaged<CFError>?
        guard let sig = SecKeyCreateSignature(key, .ecdsaSignatureMessageX962SHA256, tbs as CFData, &error) as Data? else {
            throw TVError("cannot sign: \(error!.takeRetainedValue())")
        }
        return DER.seq(tbs, ecdsaSHA256, DER.bits(sig))
    }

    /// The certificate as PEM, for whoever wants to trust it.
    public var pem: String {
        let b64 = certificate.base64EncodedString(options: [.lineLength64Characters, .endLineWithLineFeed])
        return "-----BEGIN CERTIFICATE-----\n\(b64)\n-----END CERTIFICATE-----\n"
    }
}
