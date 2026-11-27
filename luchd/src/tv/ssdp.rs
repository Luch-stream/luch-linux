//! SSDP (UPnP Device Architecture 1.1, section 1): finds DLNA renderers. An M-SEARCH goes to 239.255.255.250:1900;
//! every renderer answers by unicast with the LOCATION of its device description, which lists its services.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    time::Duration,
};

use tokio::net::UdpSocket;

use super::{xml, Control, Device, Kind};

pub const GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
pub const RENDERER: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";
const AV_TRANSPORT: &str = "urn:schemas-upnp-org:service:AVTransport:";
const RENDERING: &str = "urn:schemas-upnp-org:service:RenderingControl:";
const MAX_DESCRIPTION: usize = 256 * 1024;

/// Where to search and how far: the real group with the TTL the spec asks for (2), or, in tests, TTL 0 (the packet
/// never leaves this machine, only local sockets in the group see it).
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub to: SocketAddr,
    pub ttl: u32,
}

impl Default for Target {
    fn default() -> Self {
        Target {
            to: SocketAddr::V4(GROUP),
            ttl: 2,
        }
    }
}

/// One answer to an M-SEARCH: the headers that matter.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub location: String,
    pub usn: String,
    pub st: String,
}

pub fn m_search(st: &str, mx: u32) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: {mx}\r\nST: {st}\r\nUSER-AGENT: Linux/1 UPnP/1.1 {}\r\n\r\n",
        luch_proto::APP
    )
}

/// The headers of an SSDP message, names lowercased; None unless it is an answer (`HTTP/1.1 200`) or a NOTIFY.
pub fn parse(msg: &str) -> Option<HashMap<String, String>> {
    let mut lines = msg.split("\r\n");
    let first = lines.next()?;
    if !(first.starts_with("HTTP/1.1 200") || first.starts_with("NOTIFY ")) {
        return None;
    }
    Some(
        lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect(),
    )
}

/// Sends an M-SEARCH for renderers and gathers the answers for `wait` (one per device, by USN).
pub async fn search(target: Target, wait: Duration) -> std::io::Result<Vec<Answer>> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    sock.set_multicast_ttl_v4(target.ttl)?;
    sock.set_multicast_loop_v4(true)?;
    let mx = wait.as_secs().clamp(1, 5) as u32;
    let msg = m_search(RENDERER, mx);
    // UDP may drop one; the spec suggests sending more than once.
    for _ in 0..2 {
        sock.send_to(msg.as_bytes(), target.to).await?;
    }
    let mut found: Vec<Answer> = Vec::new();
    let mut buf = vec![0u8; 4096];
    let until = tokio::time::Instant::now() + wait;
    while let Ok(Ok((n, _))) = tokio::time::timeout_at(until, sock.recv_from(&mut buf)).await {
        let Some(h) = parse(&String::from_utf8_lossy(&buf[..n])) else {
            continue;
        };
        let (Some(location), Some(usn)) = (h.get("location"), h.get("usn")) else {
            continue;
        };
        if !found.iter().any(|a| &a.usn == usn) {
            found.push(Answer {
                location: location.clone(),
                usn: usn.clone(),
                st: h.get("st").cloned().unwrap_or_default(),
            });
        }
    }
    Ok(found)
}

/// Fetches a renderer's description and makes a device of it; None unless it has an AVTransport.
pub async fn describe(http: &reqwest::Client, location: &str) -> Result<Option<Device>, String> {
    let res = http
        .get(location)
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("{location}: HTTP {}", res.status()));
    }
    let body = res.bytes().await.map_err(|e| e.to_string())?;
    if body.len() > MAX_DESCRIPTION {
        return Err(format!("{location}: description too long"));
    }
    Ok(device(location, &String::from_utf8_lossy(&body)))
}

/// The device a description makes, its control URLs made absolute (against URLBase, else the LOCATION).
pub fn device(location: &str, desc: &str) -> Option<Device> {
    let base = xml::text(desc, "URLBase")
        .filter(|b| b.starts_with("http"))
        .unwrap_or_else(|| location.to_string());
    let mut av = None;
    let mut rendering = None;
    for s in xml::all(desc, "service") {
        let ty = xml::text(s, "serviceType").unwrap_or_default();
        let Some(url) = xml::text(s, "controlURL") else {
            continue;
        };
        let url = crate::cast::resolve(&base, &url);
        if ty.starts_with(AV_TRANSPORT) && av.is_none() {
            av = Some((url, ty));
        } else if ty.starts_with(RENDERING) && rendering.is_none() {
            rendering = Some((url, ty));
        }
    }
    let (av_transport, av_type) = av?;
    let udn = xml::text(desc, "UDN")?;
    let host = location.split("://").nth(1)?.split(['/', ':']).next()?;
    let address: IpAddr = host.parse().ok()?;
    let port = location
        .split("://")
        .nth(1)?
        .split('/')
        .next()?
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or(80);
    let model = [
        xml::text(desc, "manufacturer"),
        xml::text(desc, "modelName"),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    Some(Device {
        id: format!("dlna:{}", udn.trim_start_matches("uuid:")),
        kind: Kind::Dlna,
        name: xml::text(desc, "friendlyName").unwrap_or_else(|| "TV".into()),
        model,
        address,
        port,
        control: Control::Dlna {
            av_transport,
            av_type,
            rendering,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_and_notifies_parse_others_do_not() {
        let a = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nLocation: http://192.168.1.40:9197/dmr\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\nUSN: uuid:abc::urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n";
        let h = parse(a).unwrap();
        assert_eq!(h["location"], "http://192.168.1.40:9197/dmr");
        assert!(h["usn"].starts_with("uuid:abc"));
        assert!(parse("M-SEARCH * HTTP/1.1\r\nST: ssdp:all\r\n\r\n").is_none());
        let s = m_search(RENDERER, 2);
        assert!(s.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(s.contains("MAN: \"ssdp:discover\"\r\n") && s.ends_with("\r\n\r\n"));
    }

    #[test]
    fn a_description_gives_absolute_control_urls() {
        let desc = super::super::fake_dlna::DESCRIPTION.replace("{UDN}", "uuid:1234-5678");
        let d = device("http://192.168.1.40:9197/dmr", &desc).unwrap();
        assert_eq!(d.id, "dlna:1234-5678");
        assert_eq!(d.name, "Living room TV");
        assert_eq!(d.model, "Example Corp TV 55");
        assert_eq!(d.address.to_string(), "192.168.1.40");
        assert_eq!(d.port, 9197);
        let Control::Dlna {
            av_transport,
            rendering,
            ..
        } = d.control
        else {
            panic!()
        };
        assert_eq!(
            av_transport,
            "http://192.168.1.40:9197/upnp/control/AVTransport1"
        );
        assert_eq!(
            rendering.unwrap().0,
            "http://192.168.1.40:9197/upnp/control/RenderingControl1"
        );
        // No AVTransport: a media server or a speaker group, not for us.
        assert!(device(
            "http://192.168.1.40/",
            "<root><device><UDN>uuid:x</UDN></device></root>"
        )
        .is_none());
    }
}
