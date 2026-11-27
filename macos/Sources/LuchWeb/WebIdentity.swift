// The web receiver's HTTPS identity, kept across restarts (luchd's web_tls.rs): one key, and a certificate for
// localhost and every address of this Mac a browser has reached it at, made again only when a new one is added, so
// a browser that accepted it once is not asked again.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchTV

struct WebIdentity {
    let identity: SelfSigned
    let names: [String]

    /// Loads it from `dir`, adding `addresses` to the names it is for (a new certificate then).
    static func load(_ dir: URL, addresses: [String]) throws -> WebIdentity {
        let fm = FileManager.default
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        let keyFile = dir.appendingPathComponent("web-key.x963")
        let certFile = dir.appendingPathComponent("web-cert.der")
        let namesFile = dir.appendingPathComponent("web-names.json")
        let key: SecKey
        if let d = try? Data(contentsOf: keyFile), let k = SelfSigned.key(from: d) {
            key = k
        } else {
            let fresh = try SelfSigned(names: ["localhost"])
            key = fresh.key
            // Only this user may read the key.
            fm.createFile(atPath: keyFile.path, contents: fresh.keyData, attributes: [.posixPermissions: 0o600])
        }
        let saved = (try? Data(contentsOf: namesFile)).flatMap { try? JSONDecoder().decode([String].self, from: $0) }
        var names = saved ?? []
        for n in ["localhost", "127.0.0.1"] + addresses where !names.contains(n) { names.append(n) }
        if names == saved, let der = try? Data(contentsOf: certFile) {
            let kept = SelfSigned(key: key, certificate: der)
            if (try? kept.identity()) != nil { return WebIdentity(identity: kept, names: names) }
        }
        let made = try SelfSigned(key: key, names: names)
        try made.certificate.write(to: certFile)
        // The names last: an interrupted update is made again on the next start.
        try JSONEncoder().encode(names).write(to: namesFile)
        return WebIdentity(identity: made, names: names)
    }
}
