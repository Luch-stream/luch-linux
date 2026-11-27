// A self-signed TLS identity made here, without the keychain: a P-256 key and an X.509 v3 certificate written in DER
// by hand (the names it is for in subjectAltName), signed with that key. The web receiver serves its page with one,
// as luchd's web_tls.rs does with rcgen; the tests' fake Chromecast has one, as a real one does. The DER
// writer is LuchNet's, the one the sender certificate is made with.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import LuchNet
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

    /// UTCTime's or, from 2050, GeneralizedTime's text for `date` (RFC 5280 4.1.2.5), as DER.time takes it.
    static func time(_ date: Date) -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = TimeZone(identifier: "UTC")
        let year = Calendar(identifier: .gregorian).dateComponents(in: f.timeZone, from: date).year ?? 2000
        f.dateFormat = year < 2050 ? "yyMMddHHmmss'Z'" : "yyyyMMddHHmmss'Z'"
        return f.string(from: date)
    }

    static func certificate(key: SecKey, names: [String], commonName: String) throws -> Data {
        guard let pub = SecKeyCopyPublicKey(key), let point = SecKeyCopyExternalRepresentation(pub, nil) as Data? else {
            throw TVError("no public key")
        }
        let ecdsaSHA256 = DER.seq(DER.oid("1.2.840.10045.4.3.2"))
        let name = DER.seq(DER.tlv(0x31, DER.seq(DER.oid("2.5.4.3"), DER.utf8(commonName))))
        let spki = DER.seq(DER.seq(DER.oid("1.2.840.10045.2.1"), DER.oid("1.2.840.10045.3.1.7")), DER.bits(Array(point)))
        var altNames: [UInt8] = []
        for n in names {
            var a = in_addr()
            if inet_pton(AF_INET, n, &a) == 1 {
                altNames += DER.tlv(0x87, withUnsafeBytes(of: &a) { Array($0) }) // iPAddress
            } else {
                altNames += DER.tlv(0x82, Array(n.utf8)) // dNSName
            }
        }
        let extensions = DER.tlv(0xa3, DER.seq(
            DER.seq(DER.oid("2.5.29.17"), DER.octets(DER.tlv(0x30, altNames))),
            DER.seq(DER.oid("2.5.29.37"), DER.octets(DER.seq(DER.oid("1.3.6.1.5.5.7.3.1"))))
        ))
        var serial = [UInt8](repeating: 0, count: 16)
        _ = SecRandomCopyBytes(kSecRandomDefault, serial.count, &serial)
        serial[0] &= 0x7f
        let now = Date()
        let validity = DER.seq(DER.time(time(now.addingTimeInterval(-86400))),
                               DER.time(time(now.addingTimeInterval(10 * 365 * 86400))))
        let tbs = DER.seq(DER.tlv(0xa0, DER.int([2])), DER.int(serial), ecdsaSHA256, name, validity, name, spki, extensions)
        var error: Unmanaged<CFError>?
        guard let sig = SecKeyCreateSignature(key, .ecdsaSignatureMessageX962SHA256, Data(tbs) as CFData, &error) as Data? else {
            throw TVError("cannot sign: \(error!.takeRetainedValue())")
        }
        return Data(DER.seq(tbs, ecdsaSHA256, DER.bits(Array(sig))))
    }

    /// The certificate as PEM, for whoever wants to trust it.
    public var pem: String {
        let b64 = certificate.base64EncodedString(options: [.lineLength64Characters, .endLineWithLineFeed])
        return "-----BEGIN CERTIFICATE-----\n\(b64)\n-----END CERTIFICATE-----\n"
    }
}
