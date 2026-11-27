//! Input datagrams from the receiver (PROTOCOL.md 4h): a 20-byte header, then type-length records.
//! The records carry state, not events, so one lost datagram costs only an in-between position.

pub const KIND_INPUT: u8 = 3;
pub const VERSION: u8 = 0;
pub const HEADER: usize = 20;

pub const REC_TOUCH: u8 = 1;
pub const REC_POINTER: u8 = 2;
pub const REC_KEYS: u8 = 3;
pub const REC_GAMEPAD: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Finger,
    Stylus,
    Eraser,
}

/// One contact that is down (or a stylus hovering) now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contact {
    pub id: u8,
    pub tool: Tool,
    /// The stylus's barrel button.
    pub button: bool,
    /// Near the screen, not touching it (a stylus only).
    pub hover: bool,
    pub x: u16,
    pub y: u16,
    /// 0..65535, 0 when the receiver does not know.
    pub pressure: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pointer {
    /// bit 0 left, 1 right, 2 middle, 3 back, 4 forward.
    pub buttons: u8,
    pub abs: Option<(u16, u16)>,
    /// The wheel comes from a touchpad: continuous, not in notches.
    pub smooth: bool,
    /// Running totals since the session began, 1/16 logical pixel.
    pub rel: (i32, i32),
    /// Running totals, 120 per notch, positive down / right: (y, x).
    pub wheel: (i32, i32),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    /// Number of the first event in `events`; the next ones count up from it.
    pub first: u16,
    pub events: Vec<(u16, bool)>,
    pub held: Vec<u16>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pad {
    pub index: u8,
    pub connected: bool,
    /// SDL order: A, B, X, Y, back, guide, start, left stick, right stick, LB, RB, up, down, left, right.
    pub buttons: u32,
    pub left: (i16, i16),
    pub right: (i16, i16),
    pub triggers: (u16, u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Touch(Vec<Contact>),
    Pointer(Pointer),
    Keys(Keys),
    Gamepad(Pad),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub seq: u32,
    pub time_us: u32,
    pub token: u64,
    pub records: Vec<Record>,
}

/// `a` comes after `b` in a wrapping u32 sequence.
pub fn newer(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < 1 << 31
}

/// `a` comes after `b` in a wrapping u16 event numbering.
pub fn newer16(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 1 << 15
}

struct Rd<'a>(&'a [u8]);

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    fn i16(&mut self) -> Option<i16> {
        self.u16().map(|v| v as i16)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }
}

fn touch(r: &mut Rd) -> Option<Record> {
    let n = r.u8()?;
    let mut v = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let id = r.u8()?;
        let t = r.u8()?;
        let tool = match t & 0x3f {
            1 => Tool::Finger,
            2 => Tool::Stylus,
            3 => Tool::Eraser,
            _ => return None,
        };
        v.push(Contact {
            id,
            tool,
            button: t & 0x40 != 0,
            hover: t & 0x80 != 0 && tool != Tool::Finger,
            x: r.u16()?,
            y: r.u16()?,
            pressure: r.u16()?,
        });
    }
    Some(Record::Touch(v))
}

fn pointer(r: &mut Rd) -> Option<Record> {
    let buttons = r.u8()?;
    let flags = r.u8()?;
    let (x, y) = (r.u16()?, r.u16()?);
    Some(Record::Pointer(Pointer {
        buttons,
        abs: (flags & 1 != 0).then_some((x, y)),
        smooth: flags & 2 != 0,
        rel: (r.i32()?, r.i32()?),
        wheel: (r.i32()?, r.i32()?),
    }))
}

fn keys(r: &mut Rd) -> Option<Record> {
    let first = r.u16()?;
    let n = r.u8()?;
    let mut events = Vec::with_capacity(n as usize);
    for _ in 0..n {
        events.push((r.u16()?, r.u8()? != 0));
    }
    let h = r.u8()?;
    let mut held = Vec::with_capacity(h as usize);
    for _ in 0..h {
        held.push(r.u16()?);
    }
    Some(Record::Keys(Keys {
        first,
        events,
        held,
    }))
}

fn gamepad(r: &mut Rd) -> Option<Record> {
    Some(Record::Gamepad(Pad {
        index: r.u8()?,
        connected: r.u8()? & 1 != 0,
        buttons: r.u32()?,
        left: (r.i16()?, r.i16()?),
        right: (r.i16()?, r.i16()?),
        triggers: (r.u16()?, r.u16()?),
    }))
}

/// Parses one datagram. A record cut short or malformed spoils the whole datagram (it is garbage, not input);
/// an unknown record type is skipped.
pub fn parse(buf: &[u8]) -> Result<Packet, &'static str> {
    if buf.len() < HEADER {
        return Err("short");
    }
    if buf[0] != KIND_INPUT {
        return Err("not input");
    }
    if buf[1] != VERSION {
        return Err("unknown version");
    }
    let mut h = Rd(&buf[4..HEADER]);
    let (seq, time_us) = (h.u32().unwrap(), h.u32().unwrap());
    let token = u64::from_be_bytes(buf[12..20].try_into().unwrap());
    let mut rest = Rd(&buf[HEADER..]);
    let mut records = Vec::new();
    while !rest.0.is_empty() {
        let (ty, len) = (
            rest.u8().ok_or("bad record")?,
            rest.u8().ok_or("bad record")?,
        );
        let body = rest.take(len as usize).ok_or("record cut short")?;
        let mut r = Rd(body);
        let rec = match ty {
            REC_TOUCH => touch(&mut r),
            REC_POINTER => pointer(&mut r),
            REC_KEYS => keys(&mut r),
            REC_GAMEPAD => gamepad(&mut r),
            _ => continue,
        };
        records.push(rec.ok_or("bad record")?);
    }
    Ok(Packet {
        seq,
        time_us,
        token,
        records,
    })
}

/// The receiver's side of the format, for the tests.
#[cfg(test)]
pub fn encode(p: &Packet) -> Vec<u8> {
    let mut out = vec![KIND_INPUT, VERSION, 0, 0];
    out.extend_from_slice(&p.seq.to_be_bytes());
    out.extend_from_slice(&p.time_us.to_be_bytes());
    out.extend_from_slice(&p.token.to_be_bytes());
    for rec in &p.records {
        let mut b = Vec::new();
        let ty = match rec {
            Record::Touch(cs) => {
                b.push(cs.len() as u8);
                for c in cs {
                    let tool = match c.tool {
                        Tool::Finger => 1,
                        Tool::Stylus => 2,
                        Tool::Eraser => 3,
                    } | if c.button { 0x40 } else { 0 }
                        | if c.hover { 0x80 } else { 0 };
                    b.extend_from_slice(&[c.id, tool]);
                    for v in [c.x, c.y, c.pressure] {
                        b.extend_from_slice(&v.to_be_bytes());
                    }
                }
                REC_TOUCH
            }
            Record::Pointer(p) => {
                let flags = p.abs.is_some() as u8 | (p.smooth as u8) << 1;
                let (x, y) = p.abs.unwrap_or_default();
                b.extend_from_slice(&[p.buttons, flags]);
                b.extend_from_slice(&x.to_be_bytes());
                b.extend_from_slice(&y.to_be_bytes());
                for v in [p.rel.0, p.rel.1, p.wheel.0, p.wheel.1] {
                    b.extend_from_slice(&v.to_be_bytes());
                }
                REC_POINTER
            }
            Record::Keys(k) => {
                b.extend_from_slice(&k.first.to_be_bytes());
                b.push(k.events.len() as u8);
                for (code, down) in &k.events {
                    b.extend_from_slice(&code.to_be_bytes());
                    b.push(*down as u8);
                }
                b.push(k.held.len() as u8);
                for code in &k.held {
                    b.extend_from_slice(&code.to_be_bytes());
                }
                REC_KEYS
            }
            Record::Gamepad(g) => {
                b.extend_from_slice(&[g.index, g.connected as u8]);
                b.extend_from_slice(&g.buttons.to_be_bytes());
                for v in [g.left.0, g.left.1, g.right.0, g.right.1] {
                    b.extend_from_slice(&v.to_be_bytes());
                }
                b.extend_from_slice(&g.triggers.0.to_be_bytes());
                b.extend_from_slice(&g.triggers.1.to_be_bytes());
                REC_GAMEPAD
            }
        };
        out.push(ty);
        out.push(b.len() as u8);
        out.extend_from_slice(&b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Packet {
        Packet {
            seq: 7,
            time_us: 123_456,
            token: 0x0123_4567_89ab_cdef,
            records: vec![
                Record::Touch(vec![
                    Contact {
                        id: 0,
                        tool: Tool::Finger,
                        button: false,
                        hover: false,
                        x: 0,
                        y: 65535,
                        pressure: 0,
                    },
                    Contact {
                        id: 3,
                        tool: Tool::Stylus,
                        button: true,
                        hover: true,
                        x: 32768,
                        y: 100,
                        pressure: 4000,
                    },
                ]),
                Record::Pointer(Pointer {
                    buttons: 0b101,
                    abs: Some((10, 20)),
                    smooth: true,
                    rel: (-16, 32),
                    wheel: (-120, 240),
                }),
                Record::Keys(Keys {
                    first: 65535,
                    events: vec![(30, true), (30, false)],
                    held: vec![42],
                }),
                Record::Gamepad(Pad {
                    index: 1,
                    connected: true,
                    buttons: 1 << 14,
                    left: (-32768, 32767),
                    right: (0, -1),
                    triggers: (0, 65535),
                }),
            ],
        }
    }

    #[test]
    fn round_trips_every_record() {
        let p = sample();
        assert_eq!(parse(&encode(&p)).unwrap(), p);
    }

    #[test]
    fn header_bytes_are_as_documented() {
        let b = encode(&Packet {
            seq: 0x01020304,
            time_us: 0x0a0b0c0d,
            token: 0x1122334455667788,
            records: vec![],
        });
        assert_eq!(
            b,
            [
                3, 0, 0, 0, 1, 2, 3, 4, 0x0a, 0x0b, 0x0c, 0x0d, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
                0x77, 0x88
            ]
        );
    }

    #[test]
    fn touch_record_bytes_are_as_documented() {
        let b = encode(&Packet {
            seq: 0,
            time_us: 0,
            token: 0,
            records: vec![Record::Touch(vec![Contact {
                id: 2,
                tool: Tool::Eraser,
                button: false,
                hover: true,
                x: 0x1234,
                y: 0x5678,
                pressure: 0x9abc,
            }])],
        });
        assert_eq!(
            &b[HEADER..],
            [1, 9, 1, 2, 0x83, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]
        );
    }

    #[test]
    fn unknown_records_are_skipped_and_broken_ones_refused() {
        let mut b = encode(&sample());
        let known = parse(&b).unwrap().records.len();
        b.extend_from_slice(&[99, 3, 1, 2, 3]);
        assert_eq!(parse(&b).unwrap().records.len(), known);
        b.extend_from_slice(&[REC_POINTER, 30, 0]); // says 30 bytes, has 1
        assert_eq!(parse(&b), Err("record cut short"));
        assert_eq!(parse(&[3, 0, 0]), Err("short"));
        let mut v = encode(&sample());
        v[0] = 1;
        assert_eq!(parse(&v), Err("not input"));
        // a pointer record with too few bytes for its fields
        let mut w = encode(&Packet {
            seq: 0,
            time_us: 0,
            token: 0,
            records: vec![],
        });
        w.extend_from_slice(&[REC_POINTER, 2, 0, 0]);
        assert_eq!(parse(&w), Err("bad record"));
    }

    #[test]
    fn sequence_comparison_wraps() {
        assert!(newer(1, 0));
        assert!(newer(0, u32::MAX));
        assert!(!newer(5, 5));
        assert!(!newer(u32::MAX, 0));
        assert!(newer16(2, 65534));
        assert!(!newer16(65534, 2));
    }
}
