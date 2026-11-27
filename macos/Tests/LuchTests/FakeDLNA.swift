// A fake DLNA TV for the tests, as strict as the spec (luch-linux luchd/src/tv/fake_dlna.rs): an SSDP responder in
// the real multicast group (on loopback and a port of its own, so no real TV answers), a device description, and SOAP
// endpoints for AVTransport:1 and RenderingControl:1 that check the envelope, the SOAPAction, the arguments and the
// transport state machine (UPnP AVTransport:1 section 2.4, error codes from 2.4.x), and fetch the link the way TVs do
// (`getcontentFeatures.dlna.org`, `Range`).
// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
@testable import LuchTV

let fakeDescription = """
<?xml version="1.0" encoding="UTF-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0" xmlns:dlna="urn:schemas-dlna-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
    <dlna:X_DLNADOC>DMR-1.50</dlna:X_DLNADOC>
    <friendlyName>Living room TV</friendlyName>
    <manufacturer>Example Corp</manufacturer>
    <modelName>TV 55</modelName>
    <UDN>{UDN}</UDN>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:RenderingControl</serviceId>
        <SCPDURL>/upnp/RenderingControl1.xml</SCPDURL>
        <controlURL>/upnp/control/RenderingControl1</controlURL>
        <eventSubURL>/upnp/event/RenderingControl1</eventSubURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:ConnectionManager</serviceId>
        <SCPDURL>/upnp/ConnectionManager1.xml</SCPDURL>
        <controlURL>/upnp/control/ConnectionManager1</controlURL>
        <eventSubURL>/upnp/event/ConnectionManager1</eventSubURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:AVTransport</serviceId>
        <SCPDURL>/upnp/AVTransport1.xml</SCPDURL>
        <controlURL>/upnp/control/AVTransport1</controlURL>
        <eventSubURL>/upnp/event/AVTransport1</eventSubURL>
      </service>
    </serviceList>
  </device>
</root>
"""

private let avt = "urn:schemas-upnp-org:service:AVTransport:1"
private let rcs = "urn:schemas-upnp-org:service:RenderingControl:1"
private let envelopeNS = "http://schemas.xmlsoap.org/soap/envelope/"
private let encodingNS = "http://schemas.xmlsoap.org/soap/encoding/"

/// What the TV fetched of a link.
struct Fetch {
    var status: Int
    var headers: [String: String]
}

final class FakeAV {
    var state = "NO_MEDIA_PRESENT"
    var uri = ""
    var meta = ""
    var pos = 0.0
    var since: Double?
    var readyAt: Double?
    var dur: Double
    var volume = 20
    var muted = false
    /// Every action asked, in order.
    var actions: [String] = []
    var fetches: [Fetch] = []

    init(dur: Double) {
        self.dur = dur
    }

    /// Moves time on: a transition finishes, a playing track reaches its end.
    func advance() {
        let t = now()
        if state == "TRANSITIONING", let r = readyAt, t >= r {
            state = "PLAYING"
            since = r
            readyAt = nil
        }
        if state == "PLAYING" {
            if let s = since {
                pos += t - s
                since = t
            }
            if pos >= dur {
                state = "STOPPED"
                pos = 0
                since = nil
            }
        }
    }
}

typealias Fault = (Int, String)

final class FakeRenderer {
    let location: String
    let udn: String
    let ssdpPort: UInt16
    private let lock = NSLock()
    private let av: FakeAV
    private var http: TestHTTPServer!
    private var ssdpFD: Int32 = -1

    /// Starts one on loopback; a video it is given lasts `dur` seconds.
    init(dur: Double) {
        udn = "uuid:\(UUID().uuidString.lowercased())"
        av = FakeAV(dur: dur)
        let desc = fakeDescription.replacingOccurrences(of: "{UDN}", with: udn)
        weak var me: FakeRenderer?
        http = TestHTTPServer { r in me?.serve(r, desc) ?? TestResponse(status: 503) }
        location = "http://127.0.0.1:\(http.port)/dmr.xml"
        (ssdpFD, ssdpPort) = FakeRenderer.ssdpSocket()
        me = self
        let (fd, location, udn) = (ssdpFD, location, udn)
        Thread.detachNewThread { FakeRenderer.answerSearches(fd, location: location, udn: udn) }
    }

    deinit {
        close(ssdpFD)
    }

    /// Reads and changes the TV under its lock.
    func with<T>(_ f: (FakeAV) -> T) -> T {
        lock.lock()
        defer { lock.unlock() }
        return f(av)
    }

    /// The TV's own remote: Stop.
    func pressStop() {
        with { a in
            a.advance()
            a.state = "STOPPED"
            a.pos = 0
            a.since = nil
        }
    }

    var actions: [String] { with { $0.actions } }

    /// A socket in the SSDP group on loopback, on a port of its own.
    static func ssdpSocket() -> (Int32, UInt16) {
        let fd = socket(AF_INET, SOCK_DGRAM, 0)
        var yes: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))
        setsockopt(fd, SOL_SOCKET, SO_REUSEPORT, &yes, socklen_t(MemoryLayout<Int32>.size))
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        _ = withUnsafePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        _ = withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &len) } }
        var m = ip_mreq()
        inet_pton(AF_INET, SSDP.group, &m.imr_multiaddr)
        inet_pton(AF_INET, "127.0.0.1", &m.imr_interface)
        setsockopt(fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, &m, socklen_t(MemoryLayout<ip_mreq>.size))
        return (fd, UInt16(bigEndian: a.sin_port))
    }

    /// The SSDP side: answers M-SEARCHes for renderers (UDA 1.1 section 1.3.2), and only well-formed ones.
    static func answerSearches(_ fd: Int32, location: String, udn: String) {
        var buf = [UInt8](repeating: 0, count: 2048)
        while true {
            var from = sockaddr_in()
            var len = socklen_t(MemoryLayout<sockaddr_in>.size)
            let n = withUnsafeMutablePointer(to: &from) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { recvfrom(fd, &buf, buf.count, 0, $0, &len) }
            }
            if n <= 0 { return }
            let msg = String(decoding: buf[0..<n], as: UTF8.self)
            var lines = msg.components(separatedBy: "\r\n")
            guard lines.removeFirst() == "M-SEARCH * HTTP/1.1", msg.hasSuffix("\r\n\r\n") else { continue }
            var h: [String: String] = [:]
            for l in lines {
                guard let colon = l.firstIndex(of: ":") else { continue }
                h[l[..<colon].trimmingCharacters(in: .whitespaces).uppercased()] = l[l.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            }
            let st = h["ST"] ?? ""
            guard h["MAN"] == "\"ssdp:discover\"", h["HOST"] == "239.255.255.250:1900", h["MX"].flatMap({ Int($0) }) != nil,
                  ["ssdp:all", "upnp:rootdevice", SSDP.renderer].contains(st) else { continue }
            let reply = Array("HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nEXT:\r\nLOCATION: \(location)\r\nSERVER: Linux/5 UPnP/1.0 FakeTV/1\r\nST: \(st)\r\nUSN: \(udn)::\(SSDP.renderer)\r\n\r\n".utf8)
            _ = withUnsafePointer(to: &from) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { sendto(fd, reply, reply.count, 0, $0, len) }
            }
        }
    }

    private func serve(_ r: TestRequest, _ desc: String) -> TestResponse {
        let xml = [("Content-Type", "text/xml; charset=\"utf-8\""), ("EXT", ""), ("SERVER", "Linux/5 UPnP/1.0 FakeTV/1")]
        switch (r.method, r.path) {
        case ("GET", "/dmr.xml"): return TestResponse(status: 200, headers: xml, body: Data(desc.utf8))
        case ("POST", "/upnp/control/AVTransport1"): return soap(r, service: avt)
        case ("POST", "/upnp/control/RenderingControl1"): return soap(r, service: rcs)
        default: return TestResponse(status: 404)
        }
    }

    private func fault(_ f: Fault) -> TestResponse {
        let body = "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"\(envelopeNS)\" s:encodingStyle=\"\(encodingNS)\"><s:Body><s:Fault>"
            + "<faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError "
            + "xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>\(f.0)</errorCode><errorDescription>\(f.1)"
            + "</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>"
        return TestResponse(status: 500, headers: [("Content-Type", "text/xml; charset=\"utf-8\"")], body: Data(body.utf8))
    }

    /// Arguments each action must have, in the order of the service description.
    private static func arguments(_ action: String) -> [String]? {
        switch action {
        case "SetAVTransportURI": return ["InstanceID", "CurrentURI", "CurrentURIMetaData"]
        case "Play": return ["InstanceID", "Speed"]
        case "Pause", "Stop", "GetTransportInfo", "GetPositionInfo", "GetMediaInfo": return ["InstanceID"]
        case "Seek": return ["InstanceID", "Unit", "Target"]
        case "GetVolume", "GetMute": return ["InstanceID", "Channel"]
        case "SetVolume": return ["InstanceID", "Channel", "DesiredVolume"]
        case "SetMute": return ["InstanceID", "Channel", "DesiredMute"]
        default: return nil
        }
    }

    private func soap(_ r: TestRequest, service: String) -> TestResponse {
        guard r.headers["content-type"]?.hasPrefix("text/xml") == true else { return TestResponse(status: 415) }
        // SOAPACTION: "<service type>#<action>", quoted.
        guard let sa = r.headers["soapaction"], sa.hasPrefix("\"\(service)#"), sa.hasSuffix("\"") else {
            return fault((401, "Invalid Action"))
        }
        let action = String(sa.dropFirst(service.count + 2).dropLast())
        let body = String(decoding: r.body, as: UTF8.self)
        let wellFormed = body.contains("xmlns:s=\"\(envelopeNS)\"") && body.contains("s:encodingStyle=\"\(encodingNS)\"")
            && body.contains("<u:\(action) xmlns:u=\"\(service)\">") && body.contains("</u:\(action)>")
        guard wellFormed, let want = Self.arguments(action) else { return fault((401, "Invalid Action")) }
        let inner = String(XML.all(body, action).first ?? "")
        var args: [String: String] = [:]
        var at = inner.startIndex
        for name in want {
            // Present, and in the order the service description gives.
            guard let i = inner.range(of: "<\(name)>", range: at..<inner.endIndex) else { return fault((402, "Invalid Args")) }
            at = i.lowerBound
            args[name] = XML.text(String(inner[at...]), name) ?? ""
        }
        guard args["InstanceID"] == "0" else { return fault((718, "Invalid InstanceID")) }
        switch act(action, args) {
        case .failure(let f): return fault((f.code, f.what))
        case .success(let out):
            let o = out.map { "<\($0.0)>\(XML.escape($0.1))</\($0.0)>" }.joined()
            let b = "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"\(envelopeNS)\" s:encodingStyle=\"\(encodingNS)\"><s:Body>"
                + "<u:\(action)Response xmlns:u=\"\(service)\">\(o)</u:\(action)Response></s:Body></s:Envelope>"
            return TestResponse(status: 200, headers: [("Content-Type", "text/xml; charset=\"utf-8\"")], body: Data(b.utf8))
        }
    }

    struct UPnPFault: Error {
        let code: Int
        let what: String
        init(_ f: Fault) { (code, what) = f }
    }

    private func act(_ action: String, _ args: [String: String]) -> Result<[(String, String)], UPnPFault> {
        with { $0.actions.append(action) }
        if action == "SetAVTransportURI" {
            let uri = args["CurrentURI"]!, meta = args["CurrentURIMetaData"]!
            guard let mime = XML.attr(meta, "res", "protocolInfo")?.split(separator: ":", omittingEmptySubsequences: false)
                .dropFirst(2).first.map(String.init) else { return .failure(UPnPFault((402, "Invalid Args"))) }
            // As a TV does before it takes a link: ask for the DLNA features and the start of the file.
            let (status, headers, _) = testFetch(uri, headers: ["getcontentFeatures.dlna.org": "1",
                                                                "transferMode.dlna.org": "Streaming", "Range": "bytes=0-"])
            guard status != 0 else { return .failure(UPnPFault((716, "Resource not found"))) }
            return with { a in
                a.fetches.append(Fetch(status: status, headers: headers))
                guard (200..<300).contains(status) else { return .failure(UPnPFault((716, "Resource not found"))) }
                if let c = headers["content-type"], !c.hasPrefix(mime) { return .failure(UPnPFault((714, "Illegal MIME-type"))) }
                a.advance()
                a.uri = uri
                a.meta = meta
                a.state = "STOPPED"
                a.pos = 0
                a.since = nil
                return .success([])
            }
        }
        if ["GetVolume", "SetVolume", "GetMute", "SetMute"].contains(action), args["Channel"] != "Master" {
            return .failure(UPnPFault((402, "Invalid Args")))
        }
        return with { a in
            a.advance()
            let t = now()
            switch action {
            case "Play":
                if a.state == "NO_MEDIA_PRESENT" { return .failure(UPnPFault((701, "Transition not available"))) }
                if args["Speed"] != "1" { return .failure(UPnPFault((717, "Play speed not supported"))) }
                if a.state == "PAUSED_PLAYBACK" {
                    a.state = "PLAYING"
                    a.since = t
                } else if a.state == "STOPPED" {
                    a.state = "TRANSITIONING"
                    a.readyAt = t + 0.3
                }
                return .success([])
            case "Pause":
                guard a.state == "PLAYING" else { return .failure(UPnPFault((701, "Transition not available"))) }
                a.state = "PAUSED_PLAYBACK"
                a.since = nil
                return .success([])
            case "Stop":
                if a.state == "NO_MEDIA_PRESENT" { return .failure(UPnPFault((701, "Transition not available"))) }
                a.state = "STOPPED"
                a.pos = 0
                a.since = nil
                return .success([])
            case "Seek":
                guard ["REL_TIME", "ABS_TIME"].contains(args["Unit"]!) else { return .failure(UPnPFault((710, "Seek mode not supported"))) }
                guard ["PLAYING", "PAUSED_PLAYBACK"].contains(a.state) else { return .failure(UPnPFault((701, "Transition not available"))) }
                guard let to = parseHMS(args["Target"]!), to <= a.dur else { return .failure(UPnPFault((711, "Illegal seek target"))) }
                a.pos = to
                if a.state == "PLAYING" { a.since = t }
                return .success([])
            case "GetTransportInfo":
                return .success([("CurrentTransportState", a.state), ("CurrentTransportStatus", "OK"), ("CurrentSpeed", "1")])
            case "GetPositionInfo":
                return .success([("Track", a.uri.isEmpty ? "0" : "1"), ("TrackDuration", hms(a.dur)), ("TrackMetaData", a.meta),
                                 ("TrackURI", a.uri), ("RelTime", hms(a.pos)), ("AbsTime", hms(a.pos)),
                                 ("RelCount", "2147483647"), ("AbsCount", "2147483647")])
            case "GetMediaInfo":
                return .success([("NrTracks", "1"), ("CurrentURI", a.uri)])
            case "GetVolume":
                return .success([("CurrentVolume", String(a.volume))])
            case "SetVolume":
                guard let v = Int(args["DesiredVolume"]!), (0...100).contains(v) else { return .failure(UPnPFault((402, "Invalid Args"))) }
                a.volume = v
                return .success([])
            case "GetMute":
                return .success([("CurrentMute", a.muted ? "1" : "0")])
            case "SetMute":
                a.muted = ["1", "true"].contains(args["DesiredMute"]!)
                return .success([])
            default:
                return .failure(UPnPFault((401, "Invalid Action")))
            }
        }
    }
}
