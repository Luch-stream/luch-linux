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
//! 13..15 PSM             Bluetooth only: the L2CAP channel for a direct connection (PROTOCOL.md, "direct"), 0 = none
//! ```
//!
//! The same bytes 13.. mean the token in a card heard by sound and the PSM in one heard over Bluetooth: each way
//! decodes its own (`decode`, `decode_ble`).

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
    /// The L2CAP PSM a receiver takes a direct connection on (Bluetooth cards only).
    pub psm: Option<u16>,
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
            psm: None,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut b = MAGIC.to_vec();
        b.push(self.version << 4 | self.kind);
        b.extend(hex4(&self.id).unwrap_or_default());
        if self.addr.is_some() || self.psm.is_some() {
            let (ip, port) = self.addr.unwrap_or((Ipv4Addr::UNSPECIFIED, 0));
            b.extend(ip.octets());
            b.extend(port.to_be_bytes());
            if let Some(psm) = self.psm {
                b.extend(psm.to_be_bytes());
            } else if let Some(t) = self.token.as_deref().and_then(hex4) {
                b.extend(t);
            }
        }
        b
    }

    /// A card heard by sound or read from a QR code; None for anything that is not a card of a version this sender
    /// speaks.
    pub fn decode(b: &[u8]) -> Option<Card> {
        let mut c = Card::head(b)?;
        c.token = (b.len() >= 17).then(|| hex(&b[13..17]));
        c.checked()
    }

    /// A card heard over Bluetooth: never a token; bytes 13..15 the PSM of a receiver that offers a direct
    /// connection. Such a receiver may have no usable address (0.0.0.0, port 0): it is still a card, without `addr`.
    pub fn decode_ble(b: &[u8]) -> Option<Card> {
        let mut c = Card::head(b)?;
        c.psm = (b.len() >= 15)
            .then(|| u16::from_be_bytes([b[13], b[14]]))
            .filter(|p| *p != 0);
        c.checked()
    }

    /// Magic, version, kind, id and address; what follows them is each way's own.
    fn head(b: &[u8]) -> Option<Card> {
        if b.len() < 7 || b[..2] != MAGIC {
            return None;
        }
        let (version, kind) = (b[2] >> 4, b[2] & 15);
        if version as u32 != luch_proto::VERSION || !(kind == RECEIVER || kind == SENDER) {
            return None;
        }
        let addr = (b.len() >= 13)
            .then(|| {
                (
                    Ipv4Addr::new(b[7], b[8], b[9], b[10]),
                    u16::from_be_bytes([b[11], b[12]]),
                )
            })
            .filter(|(ip, port)| !ip.is_unspecified() && *port != 0);
        Some(Card {
            kind,
            version,
            id: hex(&b[3..7]),
            addr,
            token: None,
            psm: None,
        })
    }

    /// A receiver's card must say how to reach it: an address, or a channel for a direct connection.
    fn checked(self) -> Option<Card> {
        (self.kind != RECEIVER || self.addr.is_some() || self.psm.is_some()).then_some(self)
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
        psm: None,
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
            psm: None,
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

    /// The vector every implementation tests (the direct brief): a receiver with no LAN address that offers a
    /// direct connection on PSM 0x0080.
    #[test]
    fn a_bluetooth_card_carries_the_psm() {
        let b = [
            0x4c, 0x75, 0x01, 0x0b, 0x6f, 0x1c, 0x2a, 0, 0, 0, 0, 0, 0, 0x00, 0x80,
        ];
        let c = Card::decode_ble(&b).unwrap();
        assert_eq!(
            (c.id.as_str(), c.addr, c.psm, c.token.as_deref()),
            ("0b6f1c2a", None, Some(0x80), None)
        );
        assert_eq!(c.encode(), b);
        // The same bytes by sound are not a card: no address, and 13..15 is not a whole token.
        assert_eq!(Card::decode(&b), None);
        // A card with an address and a PSM: 15 bytes, flags 3 + manufacturer data 4 + 15 = 22 of 31.
        let both = Card {
            token: None,
            psm: Some(0x0081),
            ..tv()
        };
        assert_eq!(both.encode().len(), 15);
        assert_eq!(Card::decode_ble(&both.encode()), Some(both));
        // Receivers that predate it send 13 bytes, and PSM 0 means none: read as before.
        let old = Card {
            token: None,
            ..tv()
        };
        assert_eq!(Card::decode_ble(&old.encode()), Some(old.clone()));
        assert_eq!(
            Card::decode_ble(&[old.encode(), vec![0, 0]].concat()),
            Some(old)
        );
        assert_eq!(Card::decode_ble(&b[..13]), None);
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
