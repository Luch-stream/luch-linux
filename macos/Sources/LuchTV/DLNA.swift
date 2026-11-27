// DLNA/UPnP AV MediaRenderer: SOAP to its AVTransport (SetAVTransportURI, Play, Pause, Seek, Stop, and
// GetTransportInfo/GetPositionInfo polled once a second for the state) and RenderingControl (volume). Subtitles go
// the ways TVs read them: a second `res`, Samsung's `sec:CaptionInfoEx` (with the `CaptionInfo.sec` header on the
// video, see `prepare`) and `pv:subtitleFileUri`. The same as luchd/src/tv/dlna.rs.
// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import LuchCast
import LuchProto

public enum DLNA {
    static let pollEvery = 1.0
    /// How long a TV may take from Play to playing before it counts as failed.
    static let startWithin = 25.0
    /// PLAYING without a duration for this long: a live stream.
    static let liveAfter = 4.0
    /// One press of a volume key, in the 0..100 of RenderingControl.
    static let volumeStep = 5

    /// The SOAP request for `action` of service type `type`, as UDA 1.1 section 3.2.1 spells it.
    public static func envelope(_ type: String, _ action: String, _ args: [(String, String)]) -> String {
        let a = args.map { "<\($0.0)>\(XML.escape($0.1))</\($0.0)>" }.joined()
        return "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" "
            + "s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:\(action) xmlns:u=\"\(type)\">\(a)</u:\(action)>"
            + "</s:Body></s:Envelope>\r\n"
    }

    /// A SOAP endpoint of one service.
    struct Soap {
        let url: String
        let type: String

        /// Calls `action`: the response body, or the UPnP error (code and description) as text.
        func call(_ action: String, _ args: [(String, String)]) -> Result<String, TVError> {
            let r = HTTP.call(url, method: "POST",
                              headers: ["Content-Type": "text/xml; charset=\"utf-8\"", "SOAPAction": "\"\(type)#\(action)\""],
                              body: Data(envelope(type, action, args).utf8), timeout: 5)
            switch r {
            case .failure(let e): return .failure(TVError("\(action): \(e.message)"))
            case .success(let a):
                if (200..<300).contains(a.status) { return .success(a.text) }
                switch (XML.text(a.text, "errorCode"), XML.text(a.text, "errorDescription")) {
                case let (c?, d?): return .failure(TVError("\(action): UPnP error \(c) (\(d))"))
                case let (c?, nil): return .failure(TVError("\(action): UPnP error \(c)"))
                default: return .failure(TVError("\(action): HTTP \(a.status)"))
                }
            }
        }
    }

    /// The DIDL-Lite that describes `m` in SetAVTransportURI: title, class, the protocolInfo TVs check before playing,
    /// and the subtitles.
    public static func didl(_ m: Media) -> String {
        let mime = m.mimeOrDefault
        let cls = m.photo ? "object.item.imageItem.photo"
            : mime.hasPrefix("audio/") ? "object.item.audioItem.musicTrack" : "object.item.videoItem"
        let features = m.photo ? FileServer.dlnaPhoto : m.ours ? FileServer.dlnaStream : "*"
        let e = XML.escape
        var item = "<dc:title>\(e(m.title.isEmpty ? "Luch" : m.title))</dc:title><upnp:class>\(cls)</upnp:class>"
            + "<res protocolInfo=\"http-get:*:\(e(mime)):\(features)\">\(e(m.url))</res>"
        if let p = m.poster { item += "<upnp:albumArtURI>\(e(p))</upnp:albumArtURI>" }
        if let s = m.subs.first {
            let ty = subType(s)
            let u = e(s.url)
            item += "<res protocolInfo=\"http-get:*:\(e(s.mime.isEmpty ? "text/srt" : s.mime)):*\">\(u)</res>"
                + "<sec:CaptionInfoEx sec:type=\"\(ty)\">\(u)</sec:CaptionInfoEx>"
                + "<pv:subtitleFileUri>\(u)</pv:subtitleFileUri><pv:subtitleFileType>\(ty)</pv:subtitleFileType>"
        }
        return "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" "
            + "xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\" xmlns:dlna=\"urn:schemas-dlna-org:metadata-1-0/\" "
            + "xmlns:sec=\"http://www.sec.co.kr/\" xmlns:pv=\"http://www.pv.com/pvns/\"><item id=\"luch\" parentID=\"0\" "
            + "restricted=\"1\">\(item)</item></DIDL-Lite>"
    }

    static func subType(_ s: Sub) -> String {
        if s.mime.contains("vtt") { return "vtt" }
        if s.url.lowercased().hasSuffix(".ass") { return "ass" }
        return "srt"
    }

    /// What the file server has to add for a DLNA TV: Samsung asks the video's response for `CaptionInfo.sec`, the
    /// subtitles' link.
    public static func prepare(_ server: FileServer, _ m: Media) {
        if let s = m.subs.first { server.addHeader(url: m.url, name: "CaptionInfo.sec", value: s.url) }
    }

    /// Starts `m` on the renderer; returns once it has taken the link and been told to play.
    static func play(_ d: Device, _ m: Media, onState: StateSink?) throws -> TVSession {
        guard case let .dlna(avURL, avType, rendering) = d.control else { throw TVError("not a DLNA renderer") }
        let av = Soap(url: avURL, type: avType)
        let rc = rendering.map { Soap(url: $0.url, type: $0.type) }
        // Some TVs take a new link only when stopped; when nothing plays they answer 701, which is fine.
        _ = av.call("Stop", [("InstanceID", "0")])
        _ = try av.call("SetAVTransportURI", [("InstanceID", "0"), ("CurrentURI", m.url), ("CurrentURIMetaData", didl(m))]).get()
        _ = try av.call("Play", [("InstanceID", "0"), ("Speed", "1")]).get()
        let s = TVSession(device: d, onState: onState)
        let player = Player(av: av, rc: rc, session: s, media: m)
        Thread.detachNewThread { player.run() }
        return s
    }

    /// What the TV said last.
    struct Seen {
        var state = ""
        var pos = 0.0
        var dur: Double?
        var uri = ""
        var at: Double?

        /// Where it is by now: a playing TV moves on between polls.
        var posNow: Double {
            if state == "PLAYING", let at { return pos + (now() - at) }
            return pos
        }
    }

    static func poll(_ av: Soap) -> Result<Seen, TVError> {
        let id = [("InstanceID", "0")]
        switch (av.call("GetTransportInfo", id), av.call("GetPositionInfo", id)) {
        case let (.success(t), .success(p)):
            var state = XML.text(t, "CurrentTransportState") ?? ""
            if XML.text(t, "CurrentTransportStatus") == "ERROR_OCCURRED" { state = "ERROR" }
            return .success(Seen(state: state, pos: XML.text(p, "RelTime").flatMap(parseHMS) ?? 0,
                                 dur: XML.text(p, "TrackDuration").flatMap(parseHMS).flatMap { $0 > 0 ? $0 : nil },
                                 uri: XML.text(p, "TrackURI") ?? "", at: now()))
        case let (.failure(e), _), let (_, .failure(e)):
            return .failure(e)
        }
    }

    /// Links are equal for a TV that echoes them back percent-decoded or with its own escaping.
    static func sameLink(_ a: String, _ b: String) -> Bool {
        let norm = { (s: String) in XML.unescape(s).replacingOccurrences(of: "%20", with: " ") }
        return norm(a) == norm(b)
    }

    final class Player {
        let av: Soap
        let rc: Soap?
        let session: TVSession
        let media: Media

        init(av: Soap, rc: Soap?, session: TVSession, media: Media) {
            self.av = av
            self.rc = rc
            self.session = session
            self.media = media
        }

        /// The session: polls the TV, tells its state, does what it is asked, until it ends.
        func run() {
            let started = now()
            var seen = Seen()
            var played = false
            var playingSince: Double?
            var failures = 0
            // A start position and a paused start can only be set once the TV plays.
            var startAt: Double? = media.start > 0 ? media.start : nil
            var pauseOnce = media.paused
            var nextPoll = now()
            let tell = session.tell
            let why: String = {
                while true {
                    if let (a, reply) = session.asks.take(until: nextPoll) {
                        switch a {
                        case .leave:
                            reply.send(.success([:]))
                            return "replaced"
                        case .stop:
                            reply.send(av.call("Stop", [("InstanceID", "0")]).map { _ in [:] })
                            return "sender"
                        case .cmd(let c):
                            tell.asked()
                            reply.send(command(c, seen))
                            nextPoll = now()
                        case .volume(let v):
                            reply.send(rc.map { volume($0, v) } ?? .failure(TVError("the TV keeps its volume to itself")))
                        }
                        continue
                    }
                    nextPoll = now() + DLNA.pollEvery
                    let current: Seen
                    switch DLNA.poll(av) {
                    case .success(let s):
                        failures = 0
                        current = s
                    case .failure(let e):
                        failures += 1
                        if failures >= 3 {
                            log("dlna: \(e.message)")
                            return "gone"
                        }
                        continue
                    }
                    if played, !current.uri.isEmpty, !sameLink(current.uri, media.url) { return "replaced" }
                    if current.state == "PLAYING", playingSince == nil { playingSince = now() }
                    let state: String
                    switch current.state {
                    // Many renderers say PLAYING as soon as they are told to, and play once they know the file: until
                    // then (or for a while, for a live stream) it is still loading.
                    case "PLAYING" where current.dur == nil && now() - (playingSince ?? now()) < liveAfter:
                        state = "loading"
                    case "PLAYING":
                        played = true
                        if let t = startAt {
                            startAt = nil
                            _ = av.call("Seek", [("InstanceID", "0"), ("Unit", "REL_TIME"), ("Target", hms(t))])
                        }
                        if pauseOnce {
                            pauseOnce = false
                            _ = av.call("Pause", [("InstanceID", "0")])
                        }
                        state = "playing"
                    case "PAUSED_PLAYBACK", "PAUSED_RECORDING":
                        played = true
                        state = "paused"
                    case "ERROR":
                        tell.error("the TV could not play it")
                        return "receiver"
                    case "STOPPED" where played, "NO_MEDIA_PRESENT" where played:
                        // Stopped near the end: played out. Anywhere else: stopped on the TV.
                        if let d = seen.dur, seen.posNow >= d - 3 {
                            tell.tell("ended", pos: d, dur: d, rate: 1)
                            return "ended"
                        }
                        return "receiver"
                    case _ where now() - started > startWithin:
                        tell.error("the TV did not start playing it (it may not know this kind of file)")
                        return "receiver"
                    default:
                        state = "loading"
                    }
                    tell.tell(state, pos: current.pos, dur: current.dur, rate: 1)
                    seen = current
                }
            }()
            session.finish(why)
        }

        func command(_ c: [String: Any], _ seen: Seen) -> Result<[String: Any], TVError> {
            let id = [("InstanceID", "0")]
            let play = [("InstanceID", "0"), ("Speed", "1")]
            func seek(_ to: Double) -> Result<String, TVError> {
                let to = seen.dur.map { min(max(0, to), max(0, $0 - 1)) } ?? max(0, to)
                return av.call("Seek", [("InstanceID", "0"), ("Unit", "REL_TIME"), ("Target", hms(to))])
            }
            let number = { (k: String) in (c[k] as? NSNumber)?.doubleValue }
            let r: Result<String, TVError>
            switch c["do"] as? String ?? "" {
            case "play": r = av.call("Play", play)
            case "pause": r = av.call("Pause", id)
            case "toggle": r = seen.state == "PLAYING" ? av.call("Pause", id) : av.call("Play", play)
            case "seek":
                guard let p = number("pos") else { return .failure(TVError("seek needs pos")) }
                r = seek(p)
            case "skip":
                guard let by = number("by") else { return .failure(TVError("skip needs by")) }
                r = seek(seen.posNow + by)
            case "rate" where number("rate") == 1: r = av.call("Play", play)
            case "rate": return .failure(TVError("this TV plays at one speed only"))
            case "sub", "audio": return .failure(TVError("choose subtitles and sound tracks with the TV's remote"))
            case "state": r = .success("")
            case let d: return .failure(TVError("unknown media command \(d)"))
            }
            return r.map { _ in [:] }
        }

        /// A Luch `volume` message ({"level"}, {"step"}, {"muted"}) done over RenderingControl; the volume after it.
        func volume(_ rc: Soap, _ v: [String: Any]) -> Result<[String: Any], TVError> {
            let master = [("InstanceID", "0"), ("Channel", "Master")]
            func get() -> Result<(Int, Bool), TVError> {
                rc.call("GetVolume", master).map { body in
                    let level = XML.text(body, "CurrentVolume").flatMap { Int($0) } ?? 0
                    // Not every TV can tell its mute.
                    let muted = (try? rc.call("GetMute", master).get()).map { ["1", "true", "True"].contains(XML.text($0, "CurrentMute") ?? "") } ?? false
                    return (level, muted)
                }
            }
            func set(_ level: Int) -> Result<String, TVError> {
                rc.call("SetVolume", master + [("DesiredVolume", String(min(100, max(0, level))))])
            }
            do {
                if let l = (v["level"] as? NSNumber)?.intValue {
                    _ = try set(l).get()
                } else if let s = (v["step"] as? NSNumber)?.intValue {
                    let (l, _) = try get().get()
                    _ = try set(l + s.signum() * volumeStep).get()
                } else if let m = v["muted"] as? Bool {
                    _ = try rc.call("SetMute", master + [("DesiredMute", m ? "1" : "0")]).get()
                }
                let (level, muted) = try get().get()
                return .success(["level": level, "muted": muted])
            } catch {
                return .failure(error)
            }
        }
    }
}
