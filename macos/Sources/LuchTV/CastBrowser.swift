// Chromecasts and Google TVs announce `_googlecast._tcp` over mDNS. The TXT record names the device: `id` (stable),
// `fn` (its name), `md` (its model) and `ca`, a bit set of what it can do (1: video out). The same as
// luchd/src/tv/mdns.rs, through the DNS-SD C API as LuchNet's Discovery does for Luch receivers.
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import dnssd
import LuchProto

public enum CastDNS {
    public static let type = "_googlecast._tcp"
    /// `ca` bit: the device shows video (speakers and speaker groups do not).
    static let videoOut = 1

    /// A Chromecast from its resolved address, port and TXT; nil for one without an id or without a screen.
    public static func device(address: String, port: UInt16, txt: [String: String]) -> Device? {
        guard let id = txt["id"], !id.isEmpty else { return nil }
        let ca = txt["ca"].flatMap { Int($0) } ?? videoOut
        guard ca & videoOut != 0 else { return nil }
        return Device(id: "cast:\(id)", kind: .cast, name: txt["fn"] ?? "Chromecast", model: txt["md"] ?? "",
                      address: address, port: port, control: .cast)
    }

    static func txtMap(_ txt: UnsafeRawPointer?, length: UInt16) -> [String: String] {
        guard let txt, length > 0 else { return [:] }
        let bytes = UnsafeRawBufferPointer(start: txt, count: Int(length))
        var out: [String: String] = [:]
        var i = 0
        while i < bytes.count {
            let n = Int(bytes[i])
            i += 1
            guard i + n <= bytes.count else { break }
            let entry = String(decoding: bytes[i..<i + n], as: UTF8.self)
            i += n
            if let eq = entry.firstIndex(of: "=") { out[entry[..<eq].lowercased()] = String(entry[entry.index(after: eq)...]) }
        }
        return out
    }

    /// The IPv4 address of a `.local.` host, through the system resolver (mDNS included).
    static func ipv4(_ host: String) -> String? {
        var hints = addrinfo(ai_flags: 0, ai_family: AF_INET, ai_socktype: SOCK_STREAM, ai_protocol: 0,
                             ai_addrlen: 0, ai_canonname: nil, ai_addr: nil, ai_next: nil)
        var res: UnsafeMutablePointer<addrinfo>?
        guard getaddrinfo(host, nil, &hints, &res) == 0, let ai = res else { return nil }
        defer { freeaddrinfo(res) }
        var buf = [CChar](repeating: 0, count: Int(NI_MAXHOST))
        guard getnameinfo(ai.pointee.ai_addr, ai.pointee.ai_addrlen, &buf, socklen_t(buf.count), nil, 0, NI_NUMERICHOST) == 0 else {
            return nil
        }
        return String(cString: buf)
    }
}

private let browseReply: DNSServiceBrowseReply = { _, flags, ifIndex, err, name, _, _, ctx in
    guard let ctx, err == kDNSServiceErr_NoError, let name else { return }
    Unmanaged<CastBrowser>.fromOpaque(ctx).takeUnretainedValue().browsed(flags: flags, ifIndex: ifIndex, name: String(cString: name))
}

/// What a resolve answers about: the instance as browsed (the key for its removal) and who asked.
private final class Resolving {
    let browser: CastBrowser
    let instance: String
    var done = false
    init(_ browser: CastBrowser, _ instance: String) {
        self.browser = browser
        self.instance = instance
    }
}

private let resolveReply: DNSServiceResolveReply = { _, _, _, err, _, host, port, txtLen, txt, ctx in
    guard let ctx, err == kDNSServiceErr_NoError, let host else { return }
    let r = Unmanaged<Resolving>.fromOpaque(ctx).takeUnretainedValue()
    r.done = true
    r.browser.resolved(instance: r.instance, host: String(cString: host), port: UInt16(bigEndian: port),
                       txt: CastDNS.txtMap(txt, length: txtLen))
}

/// Follows `_googlecast._tcp` on the LAN for as long as the app runs.
final class CastBrowser {
    var onFound: ((_ instance: String, Device) -> Void)?
    var onGone: ((_ instance: String) -> Void)?
    private var browser: DNSServiceRef?

    func start() {
        let ctx = Unmanaged.passUnretained(self).toOpaque()
        guard DNSServiceBrowse(&browser, 0, 0, CastDNS.type, nil, browseReply, ctx) == kDNSServiceErr_NoError, let ref = browser else {
            log("tv: cannot browse \(CastDNS.type)")
            return
        }
        process(ref)
    }

    private func process(_ ref: DNSServiceRef) {
        Thread.detachNewThread {
            while DNSServiceProcessResult(ref) == kDNSServiceErr_NoError {}
        }
    }

    fileprivate func browsed(flags: DNSServiceFlags, ifIndex: UInt32, name: String) {
        // A VPN's tunnel is not the room the TV is in.
        var buf = [CChar](repeating: 0, count: Int(IF_NAMESIZE))
        if if_indextoname(ifIndex, &buf) != nil, String(cString: buf).hasPrefix("utun") { return }
        guard flags & kDNSServiceFlagsAdd != 0 else {
            onGone?(name)
            return
        }
        var ref: DNSServiceRef?
        let r = Resolving(self, name)
        let ctx = Unmanaged.passRetained(r).toOpaque()
        guard DNSServiceResolve(&ref, 0, ifIndex, name, CastDNS.type, "local.", resolveReply, ctx) == kDNSServiceErr_NoError,
              let ref else {
            Unmanaged<Resolving>.fromOpaque(ctx).release()
            return
        }
        // One answer is enough; the resolve is given up after a while either way.
        Thread.detachNewThread {
            let fd = DNSServiceRefSockFD(ref)
            let until = Date().addingTimeInterval(10)
            while !r.done, Date() < until {
                var p = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
                let n = poll(&p, 1, 1000)
                if n < 0 { break }
                if n > 0, DNSServiceProcessResult(ref) != kDNSServiceErr_NoError { break }
            }
            DNSServiceRefDeallocate(ref)
            Unmanaged<Resolving>.fromOpaque(ctx).release()
        }
    }

    fileprivate func resolved(instance: String, host: String, port: UInt16, txt: [String: String]) {
        // getaddrinfo can wait on the network: not on the DNS-SD thread.
        DispatchQueue.global(qos: .utility).async { [weak self] in
            guard let address = CastDNS.ipv4(host), let d = CastDNS.device(address: address, port: port, txt: txt) else { return }
            self?.onFound?(instance, d)
        }
    }
}
