//! DLNA/UPnP AV MediaRenderer: SOAP to its AVTransport (SetAVTransportURI, Play, Pause, Seek, Stop, and
//! GetTransportInfo/GetPositionInfo polled once a second for the state) and RenderingControl (volume).
//! Subtitles go the ways TVs read them: a second `res`, Samsung's `sec:CaptionInfoEx` (with the `CaptionInfo.sec`
//! header on the video, see `prepare`) and `pv:subtitleFileUri`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::{hms, parse_hms, session, xml, Ask, Control, Device, Ends, Media, Session};
use crate::cast;

const POLL: Duration = Duration::from_secs(1);
/// How long a TV may take from Play to playing before it counts as failed.
const START_WITHIN: Duration = Duration::from_secs(25);
/// PLAYING without a duration for this long: a live stream.
const LIVE_AFTER: Duration = Duration::from_secs(4);
/// One press of a volume key, in the 0..100 of RenderingControl.
const VOLUME_STEP: i64 = 5;

/// A SOAP endpoint of one service.
#[derive(Clone)]
pub struct Soap {
    http: reqwest::Client,
    url: String,
    ty: String,
}

/// The SOAP request for `action` of service type `ty`, as UDA 1.1 section 3.2.1 spells it.
pub fn envelope(ty: &str, action: &str, args: &[(&str, &str)]) -> String {
    let mut a = String::new();
    for (k, v) in args {
        a.push_str(&format!("<{k}>{}</{k}>", xml::escape(v)));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{ty}\">{a}</u:{action}>\
         </s:Body></s:Envelope>\r\n"
    )
}

impl Soap {
    pub fn new(http: &reqwest::Client, url: &str, ty: &str) -> Soap {
        Soap {
            http: http.clone(),
            url: url.to_string(),
            ty: ty.to_string(),
        }
    }

    /// Calls `action`; the response body, or the UPnP error (code and description) as text.
    pub async fn call(&self, action: &str, args: &[(&str, &str)]) -> Result<String, String> {
        let res = self
            .http
            .post(&self.url)
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header("SOAPAction", format!("\"{}#{action}\"", self.ty))
            .body(envelope(&self.ty, action, args))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("{action}: {e}"))?;
        let ok = res.status().is_success();
        let status = res.status().as_u16();
        let body = res.text().await.map_err(|e| format!("{action}: {e}"))?;
        if ok {
            return Ok(body);
        }
        Err(
            match (
                xml::text(&body, "errorCode"),
                xml::text(&body, "errorDescription"),
            ) {
                (Some(c), Some(d)) => format!("{action}: UPnP error {c} ({d})"),
                (Some(c), None) => format!("{action}: UPnP error {c}"),
                _ => format!("{action}: HTTP {status}"),
            },
        )
    }
}

/// The DIDL-Lite that describes `m` in SetAVTransportURI: title, class, the protocolInfo TVs check before playing,
/// and the subtitles.
pub fn didl(m: &Media) -> String {
    let mime = if m.mime.is_empty() {
        if m.photo {
            "image/jpeg"
        } else {
            "video/mp4"
        }
    } else {
        &m.mime
    };
    let class = if m.photo {
        "object.item.imageItem.photo"
    } else if mime.starts_with("audio/") {
        "object.item.audioItem.musicTrack"
    } else {
        "object.item.videoItem"
    };
    let features = if m.photo {
        cast::DLNA_PHOTO
    } else if m.ours {
        cast::DLNA_STREAM
    } else {
        "*"
    };
    let e = xml::escape;
    let mut item = format!(
        "<dc:title>{}</dc:title><upnp:class>{class}</upnp:class><res protocolInfo=\"http-get:*:{}:{features}\">{}</res>",
        e(if m.title.is_empty() { "Luch" } else { &m.title }),
        e(mime),
        e(&m.url)
    );
    if let Some(p) = &m.poster {
        item.push_str(&format!("<upnp:albumArtURI>{}</upnp:albumArtURI>", e(p)));
    }
    if let Some(s) = m.subs.first() {
        let ty = sub_type(s);
        item.push_str(&format!(
            "<res protocolInfo=\"http-get:*:{}:*\">{u}</res><sec:CaptionInfoEx sec:type=\"{ty}\">{u}</sec:CaptionInfoEx>\
             <pv:subtitleFileUri>{u}</pv:subtitleFileUri><pv:subtitleFileType>{ty}</pv:subtitleFileType>",
            e(if s.mime.is_empty() { "text/srt" } else { &s.mime }),
            u = e(&s.url)
        ));
    }
    format!(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\" xmlns:dlna=\"urn:schemas-dlna-org:metadata-1-0/\" \
         xmlns:sec=\"http://www.sec.co.kr/\" xmlns:pv=\"http://www.pv.com/pvns/\"><item id=\"luch\" parentID=\"0\" \
         restricted=\"1\">{item}</item></DIDL-Lite>"
    )
}

fn sub_type(s: &super::Sub) -> &'static str {
    if s.mime.contains("vtt") {
        "vtt"
    } else if s.url.to_ascii_lowercase().ends_with(".ass") {
        "ass"
    } else {
        "srt"
    }
}

/// What the file server has to add for a DLNA TV: Samsung asks the video's response for `CaptionInfo.sec`, the
/// subtitles' link.
pub fn prepare(srv: &cast::Server, m: &Media) {
    if let Some(s) = m.subs.first() {
        srv.add_header(&m.url, "CaptionInfo.sec", &s.url);
    }
}

/// Starts `m` on the renderer; returns once it has taken the link and been told to play.
pub async fn play(
    d: &Device,
    m: Media,
    http: &reqwest::Client,
) -> Result<(Session, mpsc::UnboundedReceiver<Value>), String> {
    let Control::Dlna {
        av_transport,
        av_type,
        rendering,
    } = &d.control
    else {
        return Err("not a DLNA renderer".into());
    };
    let av = Soap::new(http, av_transport, av_type);
    let rc = rendering.as_ref().map(|(u, t)| Soap::new(http, u, t));
    // Some TVs take a new link only when stopped; when nothing plays they answer 701, which is fine.
    let _ = av.call("Stop", &[("InstanceID", "0")]).await;
    av.call(
        "SetAVTransportURI",
        &[
            ("InstanceID", "0"),
            ("CurrentURI", &m.url),
            ("CurrentURIMetaData", &didl(&m)),
        ],
    )
    .await?;
    av.call("Play", &[("InstanceID", "0"), ("Speed", "1")])
        .await?;
    let (s, ends, states) = session();
    tokio::spawn(run(av, rc, ends, m));
    Ok((s, states))
}

/// What the TV said last.
#[derive(Clone, Debug, Default, PartialEq)]
struct Seen {
    state: String,
    pos: f64,
    dur: Option<f64>,
    uri: String,
    at: Option<Instant>,
}

impl Seen {
    /// Where it is by now: a playing TV moves on between polls.
    fn pos_now(&self) -> f64 {
        match (self.state.as_str(), self.at) {
            ("PLAYING", Some(t)) => self.pos + t.elapsed().as_secs_f64(),
            _ => self.pos,
        }
    }
}

async fn poll(av: &Soap) -> Result<Seen, String> {
    let t = av.call("GetTransportInfo", &[("InstanceID", "0")]).await?;
    let p = av.call("GetPositionInfo", &[("InstanceID", "0")]).await?;
    let mut state = xml::text(&t, "CurrentTransportState").unwrap_or_default();
    if xml::text(&t, "CurrentTransportStatus").as_deref() == Some("ERROR_OCCURRED") {
        state = "ERROR".into();
    }
    Ok(Seen {
        state,
        pos: xml::text(&p, "RelTime")
            .and_then(|s| parse_hms(&s))
            .unwrap_or(0.0),
        dur: xml::text(&p, "TrackDuration")
            .and_then(|s| parse_hms(&s))
            .filter(|d| *d > 0.0),
        uri: xml::text(&p, "TrackURI").unwrap_or_default(),
        at: Some(Instant::now()),
    })
}

/// The session: polls the TV, tells its state, does what it is asked, until it ends.
async fn run(av: Soap, rc: Option<Soap>, mut ends: Ends, m: Media) {
    let started = Instant::now();
    let mut seen = Seen::default();
    let mut played = false;
    let mut playing_since: Option<Instant> = None;
    let mut failures = 0;
    // A start position and a paused start can only be set once the TV plays.
    let mut start_at = (m.start > 0.0).then_some(m.start);
    let mut pause_once = m.paused;
    let mut tick = tokio::time::interval(POLL);
    let why = loop {
        tokio::select! {
            a = ends.asks.recv() => {
                let Some((a, reply)) = a else { break "replaced" };
                match a {
                    Ask::Leave => {
                        let _ = reply.send(Ok(json!({})));
                        break "replaced";
                    }
                    Ask::Stop => {
                        let r = av.call("Stop", &[("InstanceID", "0")]).await;
                        let _ = reply.send(r.map(|_| json!({})));
                        break "sender";
                    }
                    Ask::Cmd(c) => {
                        ends.tell.asked();
                        let r = command(&av, &c, &seen).await;
                        let _ = reply.send(r);
                        tick.reset_immediately();
                    }
                    Ask::Volume(v) => {
                        let _ = reply.send(match &rc {
                            Some(rc) => volume(rc, &v).await,
                            None => Err("the TV keeps its volume to itself".into()),
                        });
                    }
                }
            }
            _ = tick.tick() => {
                let now = match poll(&av).await {
                    Ok(s) => { failures = 0; s }
                    Err(e) => {
                        failures += 1;
                        if failures >= 3 {
                            eprintln!("[luchd] DLNA: {e}");
                            break "gone";
                        }
                        continue;
                    }
                };
                if played && !now.uri.is_empty() && !same_link(&now.uri, &m.url) {
                    break "replaced";
                }
                let state = match now.state.as_str() {
                    // Many renderers say PLAYING as soon as they are told to, and play once they know the
                    // file: until then (or for a while, for a live stream) it is still loading.
                    "PLAYING" if now.dur.is_none() && playing_since.get_or_insert_with(Instant::now).elapsed() < LIVE_AFTER => "loading",
                    "PLAYING" => {
                        played = true;
                        if let Some(t) = start_at.take() {
                            let _ = av.call("Seek", &[("InstanceID", "0"), ("Unit", "REL_TIME"), ("Target", &hms(t))]).await;
                        }
                        if std::mem::take(&mut pause_once) {
                            let _ = av.call("Pause", &[("InstanceID", "0")]).await;
                        }
                        "playing"
                    }
                    "PAUSED_PLAYBACK" | "PAUSED_RECORDING" => { played = true; "paused" }
                    "ERROR" => {
                        ends.tell.error("the TV could not play it");
                        break "receiver";
                    }
                    "STOPPED" | "NO_MEDIA_PRESENT" if played => {
                        // Stopped near the end: played out. Anywhere else: stopped on the TV.
                        let end = seen.dur.is_some_and(|d| seen.pos_now() >= d - 3.0);
                        if end {
                            ends.tell.tell("ended", seen.dur.unwrap_or(seen.pos), seen.dur, 1.0);
                            break "ended";
                        }
                        break "receiver";
                    }
                    _ if started.elapsed() > START_WITHIN => {
                        ends.tell.error("the TV did not start playing it (it may not know this kind of file)");
                        break "receiver";
                    }
                    _ => "loading",
                };
                ends.tell.tell(state, now.pos, now.dur, 1.0);
                seen = now;
            }
        }
    };
    ends.tell.stopped(why);
}

/// Links are equal for a TV that echoes them back percent-decoded or with its own escaping.
fn same_link(a: &str, b: &str) -> bool {
    let norm = |s: &str| xml::unescape(s).replace("%20", " ");
    norm(a) == norm(b)
}

async fn command(av: &Soap, c: &Value, seen: &Seen) -> Result<Value, String> {
    let seek = |to: f64| {
        let to = match seen.dur {
            Some(d) => to.clamp(0.0, (d - 1.0).max(0.0)),
            None => to.max(0.0),
        };
        hms(to)
    };
    let id = [("InstanceID", "0")];
    match c["do"].as_str().unwrap_or("") {
        "play" => {
            av.call("Play", &[("InstanceID", "0"), ("Speed", "1")])
                .await
        }
        "pause" => av.call("Pause", &id).await,
        "toggle" if seen.state == "PLAYING" => av.call("Pause", &id).await,
        "toggle" => {
            av.call("Play", &[("InstanceID", "0"), ("Speed", "1")])
                .await
        }
        "seek" => {
            let to = seek(c["pos"].as_f64().ok_or("seek needs pos")?);
            av.call(
                "Seek",
                &[("InstanceID", "0"), ("Unit", "REL_TIME"), ("Target", &to)],
            )
            .await
        }
        "skip" => {
            let to = seek(seen.pos_now() + c["by"].as_f64().ok_or("skip needs by")?);
            av.call(
                "Seek",
                &[("InstanceID", "0"), ("Unit", "REL_TIME"), ("Target", &to)],
            )
            .await
        }
        "rate" if c["rate"].as_f64() == Some(1.0) => {
            av.call("Play", &[("InstanceID", "0"), ("Speed", "1")])
                .await
        }
        "rate" => Err("this TV plays at one speed only".into()),
        "sub" | "audio" => Err("choose subtitles and sound tracks with the TV's remote".into()),
        "state" => Ok(String::new()),
        d => Err(format!("unknown media command {d}")),
    }
    .map(|_| json!({}))
}

/// A Luch `volume` message ({"level"}, {"step"}, {"muted"}) done over RenderingControl; the volume after it.
async fn volume(rc: &Soap, v: &Value) -> Result<Value, String> {
    let master = [("InstanceID", "0"), ("Channel", "Master")];
    let get = || async {
        let level = rc.call("GetVolume", &master).await?;
        let level: i64 = xml::text(&level, "CurrentVolume")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        // Not every TV can tell its mute.
        let muted = match rc.call("GetMute", &master).await {
            Ok(m) => matches!(
                xml::text(&m, "CurrentMute").as_deref(),
                Some("1" | "true" | "True")
            ),
            Err(_) => false,
        };
        Ok::<_, String>((level, muted))
    };
    let set = |level: i64| {
        let l = level.clamp(0, 100).to_string();
        async move {
            rc.call(
                "SetVolume",
                &[
                    ("InstanceID", "0"),
                    ("Channel", "Master"),
                    ("DesiredVolume", &l),
                ],
            )
            .await
        }
    };
    if let Some(l) = v["level"].as_i64() {
        set(l).await?;
    } else if let Some(s) = v["step"].as_i64() {
        let (l, _) = get().await?;
        set(l + s.signum() * VOLUME_STEP).await?;
    } else if let Some(m) = v["muted"].as_bool() {
        let m = if m { "1" } else { "0" };
        rc.call(
            "SetMute",
            &[
                ("InstanceID", "0"),
                ("Channel", "Master"),
                ("DesiredMute", m),
            ],
        )
        .await?;
    }
    let (level, muted) = get().await?;
    Ok(json!({"level": level, "muted": muted}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_is_the_one_uda_spells() {
        let e = envelope(
            "urn:schemas-upnp-org:service:AVTransport:1",
            "Seek",
            &[
                ("InstanceID", "0"),
                ("Unit", "REL_TIME"),
                ("Target", "0:01:30"),
            ],
        );
        assert!(e.contains("<u:Seek xmlns:u=\"urn:schemas-upnp-org:service:AVTransport:1\"><InstanceID>0</InstanceID><Unit>REL_TIME</Unit><Target>0:01:30</Target></u:Seek>"), "{e}");
        assert!(e.contains("s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\""));
    }

    #[test]
    fn didl_carries_type_features_and_subtitles() {
        let m = Media {
            url: "http://192.168.1.20:47810/t/Film & Co.mkv".into(),
            mime: "video/x-matroska".into(),
            title: "Film & Co".into(),
            ours: true,
            subs: vec![super::super::Sub {
                url: "http://192.168.1.20:47810/u/film.en.srt".into(),
                mime: "application/x-subrip".into(),
                lang: "en".into(),
            }],
            ..Default::default()
        };
        let d = didl(&m);
        assert!(d.contains("<dc:title>Film &amp; Co</dc:title>"));
        assert!(d.contains("<upnp:class>object.item.videoItem</upnp:class>"));
        assert!(d.contains(&format!(
            "protocolInfo=\"http-get:*:video/x-matroska:{}\"",
            cast::DLNA_STREAM
        )));
        assert!(d.contains("<sec:CaptionInfoEx sec:type=\"srt\">http://192.168.1.20:47810/u/film.en.srt</sec:CaptionInfoEx>"));
        let photo = didl(&Media {
            url: "http://x/a.jpg".into(),
            mime: "image/jpeg".into(),
            photo: true,
            ..Default::default()
        });
        assert!(photo.contains("object.item.imageItem.photo") && photo.contains(cast::DLNA_PHOTO));
        let song = didl(&Media {
            url: "http://x/a.mp3".into(),
            mime: "audio/mpeg".into(),
            ..Default::default()
        });
        assert!(
            song.contains("object.item.audioItem.musicTrack") && song.contains("audio/mpeg:*\"")
        );
    }

    #[test]
    fn times_go_both_ways() {
        assert_eq!(hms(3723.9), "1:02:03");
        assert_eq!(parse_hms("1:02:03"), Some(3723.0));
        assert_eq!(parse_hms("00:00:05.500"), Some(5.5));
        assert_eq!(parse_hms("0:00:01.1/2"), Some(1.5));
        assert_eq!(parse_hms("NOT_IMPLEMENTED"), None);
        assert_eq!(parse_hms(""), None);
    }
}
