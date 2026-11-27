//! A fake DLNA TV for the tests, as strict as the spec: an SSDP responder in the real multicast group, a device
//! description, and SOAP endpoints for AVTransport:1 and RenderingControl:1 that check the envelope, the SOAPAction,
//! the arguments and the transport state machine (UPnP AVTransport:1 section 2.4, error codes from 2.4.x), and fetch
//! the link the way TVs do (`getcontentFeatures.dlna.org`, `Range`).

use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, UdpSocket},
};

use super::{hms, parse_hms, ssdp, xml};

pub const DESCRIPTION: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
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
</root>"#;

const AVT: &str = "urn:schemas-upnp-org:service:AVTransport:1";
const RC: &str = "urn:schemas-upnp-org:service:RenderingControl:1";
const ENVELOPE: &str = "http://schemas.xmlsoap.org/soap/envelope/";
const ENCODING: &str = "http://schemas.xmlsoap.org/soap/encoding/";

/// What the TV fetched of a link.
#[derive(Clone, Debug)]
pub struct Fetch {
    pub status: u16,
    pub headers: HashMap<String, String>,
}

#[derive(Debug)]
pub struct Av {
    pub state: &'static str,
    pub uri: String,
    pub meta: String,
    pos: f64,
    since: Option<Instant>,
    ready_at: Option<Instant>,
    pub dur: f64,
    pub volume: i64,
    pub muted: bool,
    /// Every action asked, in order.
    pub actions: Vec<String>,
    pub fetches: Vec<Fetch>,
}

impl Av {
    /// Moves time on: a transition finishes, a playing track reaches its end.
    fn advance(&mut self) {
        let now = Instant::now();
        if self.state == "TRANSITIONING" && self.ready_at.is_some_and(|t| now >= t) {
            self.state = "PLAYING";
            self.since = self.ready_at.take();
        }
        if self.state == "PLAYING" {
            if let Some(s) = self.since {
                self.pos += now.duration_since(s).as_secs_f64();
                self.since = Some(now);
            }
            if self.pos >= self.dur {
                self.state = "STOPPED";
                self.pos = 0.0;
                self.since = None;
            }
        }
    }
}

pub struct FakeRenderer {
    pub location: String,
    pub udn: String,
    pub av: Arc<Mutex<Av>>,
}

type Fault = (u16, &'static str);

impl FakeRenderer {
    /// Starts one on loopback, answering M-SEARCHes in the real group on port 1900.
    pub async fn start(dur: f64) -> FakeRenderer {
        let udn = format!("uuid:{}", uuid::Uuid::new_v4());
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let location = format!("http://{}/dmr.xml", l.local_addr().unwrap());
        let av = Arc::new(Mutex::new(Av {
            state: "NO_MEDIA_PRESENT",
            uri: String::new(),
            meta: String::new(),
            pos: 0.0,
            since: None,
            ready_at: None,
            dur,
            volume: 20,
            muted: false,
            actions: Vec::new(),
            fetches: Vec::new(),
        }));
        let (desc, a) = (DESCRIPTION.replace("{UDN}", &udn), av.clone());
        tokio::spawn(async move {
            loop {
                let (s, _) = l.accept().await.unwrap();
                let (desc, a) = (desc.clone(), a.clone());
                tokio::spawn(async move {
                    let _ = serve(s, &desc, &a).await;
                });
            }
        });
        tokio::spawn(answer_searches(location.clone(), udn.clone()));
        FakeRenderer { location, udn, av }
    }

    /// The TV's own remote: Stop.
    pub fn press_stop(&self) {
        let mut a = self.av.lock().unwrap();
        a.advance();
        a.state = "STOPPED";
        a.pos = 0.0;
        a.since = None;
    }

    pub fn actions(&self) -> Vec<String> {
        self.av.lock().unwrap().actions.clone()
    }
}

/// The SSDP side: answers M-SEARCHes for renderers (UDA 1.1 section 1.3.2), and only well-formed ones.
async fn answer_searches(location: String, udn: String) {
    let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None).unwrap();
    s.set_reuse_address(true).unwrap();
    s.set_reuse_port(true).unwrap();
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 1900)).into())
        .unwrap();
    s.join_multicast_v4(ssdp::GROUP.ip(), &Ipv4Addr::UNSPECIFIED)
        .unwrap();
    s.set_nonblocking(true).unwrap();
    let sock = UdpSocket::from_std(s.into()).unwrap();
    let mut buf = vec![0u8; 2048];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf).await else {
            return;
        };
        let msg = String::from_utf8_lossy(&buf[..n]).to_string();
        let mut lines = msg.split("\r\n");
        if lines.next() != Some("M-SEARCH * HTTP/1.1") || !msg.ends_with("\r\n\r\n") {
            continue;
        }
        let h: HashMap<String, String> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_uppercase(), v.trim().to_string()))
            .collect();
        let st = h.get("ST").cloned().unwrap_or_default();
        let wanted = ["ssdp:all", "upnp:rootdevice", ssdp::RENDERER].contains(&st.as_str());
        if h.get("MAN").map(String::as_str) != Some("\"ssdp:discover\"")
            || h.get("HOST").map(String::as_str) != Some("239.255.255.250:1900")
            || h.get("MX").and_then(|m| m.parse::<u32>().ok()).is_none()
            || !wanted
        {
            continue;
        }
        let reply = format!(
            "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nEXT:\r\nLOCATION: {location}\r\nSERVER: Linux/5 UPnP/1.0 FakeTV/1\r\nST: {st}\r\nUSN: {udn}::{}\r\n\r\n",
            ssdp::RENDERER
        );
        let _ = sock.send_to(reply.as_bytes(), from).await;
    }
}

async fn serve(s: tokio::net::TcpStream, desc: &str, av: &Mutex<Av>) -> std::io::Result<()> {
    let (rd, mut wr) = s.into_split();
    let mut rd = BufReader::new(rd);
    loop {
        let mut first = String::new();
        if rd.read_line(&mut first).await? == 0 {
            return Ok(());
        }
        let mut h = HashMap::new();
        loop {
            let mut l = String::new();
            rd.read_line(&mut l).await?;
            let l = l.trim_end();
            if l.is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                h.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        let len: usize = h
            .get("content-length")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; len];
        rd.read_exact(&mut body).await?;
        let mut parts = first.split_whitespace();
        let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let (code, ctype, out) = match (method, path) {
            ("GET", "/dmr.xml") => (200, "text/xml; charset=\"utf-8\"", desc.to_string()),
            ("POST", "/upnp/control/AVTransport1") => {
                soap(&h, &String::from_utf8_lossy(&body), AVT, av).await
            }
            ("POST", "/upnp/control/RenderingControl1") => {
                soap(&h, &String::from_utf8_lossy(&body), RC, av).await
            }
            _ => (404, "text/plain", String::new()),
        };
        let head = format!(
            "HTTP/1.1 {code} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nEXT:\r\nSERVER: Linux/5 UPnP/1.0 FakeTV/1\r\n\r\n",
            out.len()
        );
        wr.write_all(head.as_bytes()).await?;
        wr.write_all(out.as_bytes()).await?;
    }
}

fn fault((code, what): Fault) -> (u16, &'static str, String) {
    (
        500,
        "text/xml; charset=\"utf-8\"",
        format!(
            "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"{ENVELOPE}\" s:encodingStyle=\"{ENCODING}\"><s:Body><s:Fault>\
             <faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError \
             xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>{code}</errorCode><errorDescription>{what}\
             </errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>"
        ),
    )
}

/// Arguments each action must have, in the order of the service description.
fn arguments(action: &str) -> Option<&'static [&'static str]> {
    Some(match action {
        "SetAVTransportURI" => &["InstanceID", "CurrentURI", "CurrentURIMetaData"],
        "Play" => &["InstanceID", "Speed"],
        "Pause" | "Stop" | "GetTransportInfo" | "GetPositionInfo" | "GetMediaInfo" => {
            &["InstanceID"]
        }
        "Seek" => &["InstanceID", "Unit", "Target"],
        "GetVolume" | "GetMute" => &["InstanceID", "Channel"],
        "SetVolume" => &["InstanceID", "Channel", "DesiredVolume"],
        "SetMute" => &["InstanceID", "Channel", "DesiredMute"],
        _ => return None,
    })
}

async fn soap(
    h: &HashMap<String, String>,
    body: &str,
    service: &str,
    av: &Mutex<Av>,
) -> (u16, &'static str, String) {
    if !h
        .get("content-type")
        .is_some_and(|c| c.starts_with("text/xml"))
    {
        return (415, "text/plain", String::new());
    }
    // SOAPACTION: "<service type>#<action>", quoted.
    let Some(action) = h.get("soapaction").and_then(|a| {
        a.strip_prefix('"')?
            .strip_suffix('"')?
            .strip_prefix(service)?
            .strip_prefix('#')
    }) else {
        return fault((401, "Invalid Action"));
    };
    let action = action.to_string();
    let well_formed = body.contains(&format!("xmlns:s=\"{ENVELOPE}\""))
        && body.contains(&format!("s:encodingStyle=\"{ENCODING}\""))
        && body.contains(&format!("<u:{action} xmlns:u=\"{service}\">"))
        && body.contains(&format!("</u:{action}>"));
    let Some(want) = arguments(&action).filter(|_| well_formed) else {
        return fault((401, "Invalid Action"));
    };
    let inner = xml::all(body, &action).first().copied().unwrap_or("");
    let mut args = HashMap::new();
    let mut at = 0;
    for name in want {
        // Present, and in the order the service description gives.
        let Some(i) = inner[at..].find(&format!("<{name}>")) else {
            return fault((402, "Invalid Args"));
        };
        at += i;
        args.insert(*name, xml::text(&inner[at..], name).unwrap_or_default());
    }
    if args["InstanceID"] != "0" {
        return fault((718, "Invalid InstanceID"));
    }
    let result = act(&action, &args, av).await;
    match result {
        Ok(out) => {
            let mut o = String::new();
            for (k, v) in out {
                o.push_str(&format!("<{k}>{}</{k}>", xml::escape(&v)));
            }
            (
                200,
                "text/xml; charset=\"utf-8\"",
                format!(
                    "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"{ENVELOPE}\" s:encodingStyle=\"{ENCODING}\"><s:Body>\
                     <u:{action}Response xmlns:u=\"{service}\">{o}</u:{action}Response></s:Body></s:Envelope>"
                ),
            )
        }
        Err(f) => fault(f),
    }
}

async fn act(
    action: &str,
    args: &HashMap<&str, String>,
    av: &Mutex<Av>,
) -> Result<Vec<(&'static str, String)>, Fault> {
    av.lock().unwrap().actions.push(action.to_string());
    if action == "SetAVTransportURI" {
        let uri = args["CurrentURI"].clone();
        let meta = args["CurrentURIMetaData"].clone();
        let mime = xml::attr(&meta, "res", "protocolInfo")
            .and_then(|p| p.split(':').nth(2).map(String::from))
            .ok_or((402, "Invalid Args"))?;
        // As a TV does before it takes a link: ask for the DLNA features and the start of the file.
        let res = reqwest::Client::new()
            .get(&uri)
            .header("getcontentFeatures.dlna.org", "1")
            .header("transferMode.dlna.org", "Streaming")
            .header("Range", "bytes=0-")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|_| (716, "Resource not found"))?;
        let fetch = Fetch {
            status: res.status().as_u16(),
            headers: res
                .headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_ascii_lowercase(),
                        v.to_str().unwrap_or("").to_string(),
                    )
                })
                .collect(),
        };
        let mut a = av.lock().unwrap();
        a.fetches.push(fetch.clone());
        if !(200..300).contains(&fetch.status) {
            return Err((716, "Resource not found"));
        }
        if fetch
            .headers
            .get("content-type")
            .is_some_and(|c| !c.starts_with(&mime))
        {
            return Err((714, "Illegal MIME-type"));
        }
        a.advance();
        a.uri = uri;
        a.meta = meta;
        a.state = "STOPPED";
        a.pos = 0.0;
        a.since = None;
        return Ok(vec![]);
    }
    let mut a = av.lock().unwrap();
    a.advance();
    let now = Instant::now();
    match action {
        "Play" => {
            if a.state == "NO_MEDIA_PRESENT" {
                return Err((701, "Transition not available"));
            }
            if args["Speed"] != "1" {
                return Err((717, "Play speed not supported"));
            }
            match a.state {
                "PAUSED_PLAYBACK" => {
                    a.state = "PLAYING";
                    a.since = Some(now);
                }
                "STOPPED" => {
                    a.state = "TRANSITIONING";
                    a.ready_at = Some(now + Duration::from_millis(300));
                }
                _ => {}
            }
            Ok(vec![])
        }
        "Pause" => {
            if a.state != "PLAYING" {
                return Err((701, "Transition not available"));
            }
            a.state = "PAUSED_PLAYBACK";
            a.since = None;
            Ok(vec![])
        }
        "Stop" => {
            if a.state == "NO_MEDIA_PRESENT" {
                return Err((701, "Transition not available"));
            }
            a.state = "STOPPED";
            a.pos = 0.0;
            a.since = None;
            Ok(vec![])
        }
        "Seek" => {
            if !["REL_TIME", "ABS_TIME"].contains(&args["Unit"].as_str()) {
                return Err((710, "Seek mode not supported"));
            }
            if !["PLAYING", "PAUSED_PLAYBACK"].contains(&a.state) {
                return Err((701, "Transition not available"));
            }
            let t = parse_hms(&args["Target"]).ok_or((711, "Illegal seek target"))?;
            if t > a.dur {
                return Err((711, "Illegal seek target"));
            }
            a.pos = t;
            if a.state == "PLAYING" {
                a.since = Some(now);
            }
            Ok(vec![])
        }
        "GetTransportInfo" => Ok(vec![
            ("CurrentTransportState", a.state.to_string()),
            ("CurrentTransportStatus", "OK".into()),
            ("CurrentSpeed", "1".into()),
        ]),
        "GetPositionInfo" => Ok(vec![
            ("Track", if a.uri.is_empty() { "0" } else { "1" }.into()),
            ("TrackDuration", hms(a.dur)),
            ("TrackMetaData", a.meta.clone()),
            ("TrackURI", a.uri.clone()),
            ("RelTime", hms(a.pos)),
            ("AbsTime", hms(a.pos)),
            ("RelCount", "2147483647".into()),
            ("AbsCount", "2147483647".into()),
        ]),
        "GetMediaInfo" => Ok(vec![
            ("NrTracks", "1".into()),
            ("CurrentURI", a.uri.clone()),
        ]),
        "GetVolume" | "SetVolume" | "GetMute" | "SetMute" if args["Channel"] != "Master" => {
            Err((402, "Invalid Args"))
        }
        "GetVolume" => Ok(vec![("CurrentVolume", a.volume.to_string())]),
        "SetVolume" => {
            let v: i64 = args["DesiredVolume"]
                .parse()
                .map_err(|_| (402, "Invalid Args"))?;
            if !(0..=100).contains(&v) {
                return Err((402, "Invalid Args"));
            }
            a.volume = v;
            Ok(vec![])
        }
        "GetMute" => Ok(vec![(
            "CurrentMute",
            if a.muted { "1" } else { "0" }.into(),
        )]),
        "SetMute" => {
            a.muted = matches!(args["DesiredMute"].as_str(), "1" | "true");
            Ok(vec![])
        }
        _ => Err((401, "Invalid Action")),
    }
}
