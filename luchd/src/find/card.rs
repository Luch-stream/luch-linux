//! The card: what a receiver says about itself where there is no room for JSON (PROTOCOL.md 1c, 1d), and the
//! QR link (1e). Bluetooth LE and sound carry the same bytes.
//!
//! ```text
//! 0..2   "Lu"            magic
//! 2      v << 4 | kind   protocol version, 1 = a receiver here, 2 = a sender looking for one
//! 3..7   id              the first 4 bytes of the id (its first 8 hex digits)
//! 7..11  IPv4            receiver only
//! 11..13 port            receiver only, big-endian
//! 13..17 token           a one-time pairing token: sound only, never over Bluetooth
//! ```

use std::net::Ipv4Addr;

pub const MAGIC: [u8; 2] = *b"Lu";
pub const RECEIVER: u8 = 1;
pub const SENDER: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub kind: u8,
    pub version: u8,
    /// 8 lowercase hex digits: the start of the id, enough to tell receivers apart and to check the welcome.
    pub id: String,
    pub addr: Option<(Ipv4Addr, u16)>,
    /// 8 lowercase hex digits.
    pub token: Option<String>,
}

fn hex4(s: &str) -> Option<[u8; 4]> {
    let s: String = s.chars().filter(|c| *c != '-').take(8).collect();
    if s.len() != 8 {
        return None;
    }
    let mut out = [0u8; 4];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

impl Card {
    /// A sender looking for receivers: only who is looking.
    pub fn looking(sender_id: &str) -> Self {
        Card {
            kind: SENDER,
            version: luch_proto::VERSION as u8,
            id: sender_id.chars().filter(|c| *c != '-').take(8).collect(),
            addr: None,
            token: None,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut b = MAGIC.to_vec();
        b.push(self.version << 4 | self.kind);
        b.extend(hex4(&self.id).unwrap_or_default());
        if let Some((ip, port)) = self.addr {
            b.extend(ip.octets());
            b.extend(port.to_be_bytes());
            if let Some(t) = self.token.as_deref().and_then(hex4) {
                b.extend(t);
            }
        }
        b
    }

    /// None for anything that is not a card of a version this sender speaks.
    pub fn decode(b: &[u8]) -> Option<Card> {
        if b.len() < 7 || b[..2] != MAGIC {
            return None;
        }
        let (version, kind) = (b[2] >> 4, b[2] & 15);
        if version as u32 != luch_proto::VERSION || !(kind == RECEIVER || kind == SENDER) {
            return None;
        }
        let id = hex(&b[3..7]);
        let addr = (b.len() >= 13).then(|| {
            (
                Ipv4Addr::new(b[7], b[8], b[9], b[10]),
                u16::from_be_bytes([b[11], b[12]]),
            )
        });
        if kind == RECEIVER && addr.is_none_or(|(ip, port)| ip.is_unspecified() || port == 0) {
            return None;
        }
        let token = (b.len() >= 17).then(|| hex(&b[13..17]));
        Some(Card {
            kind,
            version,
            id,
            addr,
            token,
        })
    }
}

/// `luch://connect?host=<ip>&port=<port>&id=<8 hex>&t=<token>` (what the receiver's QR code says), or a plain
/// `host[:port]` typed by a person. The phone app reads the same link; unknown parameters are ignored.
pub fn parse_link(s: &str) -> Option<Card> {
    let s = s.trim();
    let (mut host, mut port, mut id, mut token) =
        (None, luch_proto::CONTROL_PORT, String::new(), None);
    if let Some(query) = s.strip_prefix("luch://connect?") {
        for kv in query.split('&') {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            match k {
                "host" => host = v.parse::<Ipv4Addr>().ok(),
                "port" => port = v.parse().ok()?,
                "id" => id = v.to_ascii_lowercase(),
                "t" => token = Some(v.to_ascii_lowercase()).filter(|t| hex4(t).is_some()),
                _ => {}
            }
        }
    } else {
        let (h, p) = match s.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().ok()?),
            None => (s, port),
        };
        host = h.parse().ok();
        port = p;
    }
    let host = host.filter(|h| !h.is_unspecified())?;
    (port != 0).then_some(Card {
        kind: RECEIVER,
        version: luch_proto::VERSION as u8,
        id,
        addr: Some((host, port)),
        token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tv() -> Card {
        Card {
            kind: RECEIVER,
            version: 0,
            id: "3f0c2a8e".into(),
            addr: Some((Ipv4Addr::new(192, 168, 1, 20), 47800)),
            token: Some("0badf00d".into()),
        }
    }

    #[test]
    fn cards_round_trip_and_fit_an_advertisement() {
        let t = tv();
        let b = t.encode();
        assert_eq!(
            super::super::modem::hex(&b),
            "4c75013f0c2a8ec0a80114bab80badf00d"
        );
        assert_eq!(Card::decode(&b), Some(t.clone()));
        // Over Bluetooth there is no token: 13 bytes, which with the company id and the flags is 20 of 31.
        let ble = Card { token: None, ..t };
        assert_eq!(ble.encode().len(), 13);
        assert_eq!(Card::decode(&ble.encode()), Some(ble));
        let me = Card::looking("9a1b2c3d-0000-4000-8000-000000000000");
        assert_eq!(me.encode(), vec![0x4c, 0x75, 0x02, 0x9a, 0x1b, 0x2c, 0x3d]);
        assert_eq!(Card::decode(&me.encode()), Some(me));
    }

    #[test]
    fn strangers_are_not_cards() {
        assert_eq!(Card::decode(b"Lu"), None);
        assert_eq!(Card::decode(&[0x4c, 0x76, 1, 0, 0, 0, 0]), None);
        // A future version, an unknown kind, a receiver with no address.
        assert_eq!(
            Card::decode(&[0x4c, 0x75, 0x11, 1, 2, 3, 4, 10, 0, 0, 1, 0, 80]),
            None
        );
        assert_eq!(Card::decode(&[0x4c, 0x75, 0x07, 1, 2, 3, 4]), None);
        assert_eq!(Card::decode(&[0x4c, 0x75, 0x01, 1, 2, 3, 4]), None);
    }

    #[test]
    fn links_from_the_qr_code_and_from_a_person() {
        let c =
            parse_link("luch://connect?host=192.168.1.20&port=47800&id=3F0C2A8E&t=0badf00d&new=1")
                .unwrap();
        assert_eq!(c, tv());
        // The link of receivers made before tokens still works.
        let old = parse_link("luch://connect?host=192.168.1.20&port=47801").unwrap();
        assert_eq!(
            (old.addr, old.token, old.id.as_str()),
            (Some((Ipv4Addr::new(192, 168, 1, 20), 47801)), None, "")
        );
        assert_eq!(
            parse_link("192.168.1.21").unwrap().addr,
            Some((Ipv4Addr::new(192, 168, 1, 21), 47800))
        );
        assert_eq!(
            parse_link(" 192.168.1.21:5000 ").unwrap().addr,
            Some((Ipv4Addr::new(192, 168, 1, 21), 5000))
        );
        for bad in [
            "",
            "luch://connect?port=1",
            "tv.local",
            "192.168.1.300",
            "192.168.1.2:0",
            "http://x/",
        ] {
            assert_eq!(parse_link(bad), None, "{bad}");
        }
    }
}
