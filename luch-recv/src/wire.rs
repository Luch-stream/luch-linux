//! The UDP packet of PROTOCOL.md section 4 and the reassembly of frames from shards.

use std::time::Instant;

pub const HEADER: usize = 24;
pub const KIND_VIDEO: u8 = 1;
pub const F_IDR: u8 = 1;
pub const F_PARITY: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: u8,
    pub flags: u8,
    pub index: u16,
    pub data_shards: u16,
    pub parity_shards: u16,
    pub seq: u32,
    pub pts_us: u32,
    pub len: u32,
    /// Capture to send on the sender, in 0.1 ms.
    pub host: u16,
}

impl Header {
    /// The header and the shard that follows it.
    pub fn parse(b: &[u8]) -> Option<(Header, &[u8])> {
        if b.len() < HEADER {
            return None;
        }
        let u16at = |o: usize| u16::from_be_bytes([b[o], b[o + 1]]);
        let u32at = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let h = Header {
            kind: b[0],
            flags: b[1],
            index: u16at(2),
            data_shards: u16at(4),
            parity_shards: u16at(6),
            seq: u32at(8),
            pts_us: u32at(12),
            len: u32at(16),
            host: u16at(20),
        };
        Some((h, &b[HEADER..]))
    }
}

pub struct Frame {
    pub seq: u32,
    #[allow(dead_code)] // for the audio sync, later
    pub pts_us: u32,
    #[allow(dead_code)]
    pub idr: bool,
    pub host: u16,
    pub data: Vec<u8>,
}

#[derive(Default)]
pub struct Push {
    pub frame: Option<Frame>,
    /// An IDR is needed and has not been asked for yet.
    pub want_keyframe: bool,
}

struct Cur {
    hdr: Header,
    shards: Vec<Option<Vec<u8>>>,
    got: usize,
}

/// Frames from shards, the way the protocol's receiver rules say: a frame goes out the moment its last shard is in, a
/// frame is lost when a packet of a later frame arrives, after a loss everything is dropped until an IDR.
pub struct Assembler {
    cur: Option<Cur>,
    last_done: Option<u32>,
    wait_idr: bool,
    asked: bool,
    asked_at: Option<Instant>,
    pub lost: u32,
}

/// `a` is later than `b`, with the counter wrapping.
fn later(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

impl Assembler {
    pub fn new() -> Self {
        Assembler {
            cur: None,
            last_done: None,
            wait_idr: true,
            asked: true,
            asked_at: Some(Instant::now()),
            lost: 0,
        }
    }

    pub fn push(&mut self, h: &Header, data: &[u8]) -> Push {
        let mut out = Push::default();
        if h.kind != KIND_VIDEO
            || h.flags & F_PARITY != 0
            || h.data_shards == 0
            || h.data_shards > 4096
        {
            return out;
        }
        if self.last_done.is_some_and(|d| !later(h.seq, d)) {
            return out; // an old or repeated frame
        }
        match &self.cur {
            Some(c) if c.hdr.seq == h.seq => {}
            Some(c) if later(h.seq, c.hdr.seq) => {
                self.lost += 1;
                self.wait_idr = true;
                self.cur = None;
            }
            Some(_) => return out, // a late shard of a frame already given up
            None => {}
        }
        let cur = self.cur.get_or_insert_with(|| Cur {
            hdr: *h,
            shards: vec![None; h.data_shards as usize],
            got: 0,
        });
        let i = h.index as usize;
        if i < cur.shards.len() && cur.shards[i].is_none() {
            cur.shards[i] = Some(data.to_vec());
            cur.got += 1;
        }
        if cur.got < cur.shards.len() {
            return out;
        }
        let c = self.cur.take().unwrap();
        self.last_done = Some(c.hdr.seq);
        let mut data: Vec<u8> = c.shards.into_iter().flatten().flatten().collect();
        data.truncate(c.hdr.len as usize);
        let idr = c.hdr.flags & F_IDR != 0;
        if self.wait_idr && !idr {
            if !self.asked {
                self.asked = true;
                self.asked_at = Some(Instant::now());
                out.want_keyframe = true;
            }
            return out;
        }
        if idr {
            self.wait_idr = false;
            self.asked = false;
            self.asked_at = None;
        }
        out.frame = Some(Frame {
            seq: c.hdr.seq,
            pts_us: c.hdr.pts_us,
            idr,
            host: c.hdr.host,
            data,
        });
        out
    }

    /// Whether an IDR was asked for a while ago and has not come: ask again.
    pub fn retry_keyframe(&mut self, after_ms: u128) -> bool {
        if self.wait_idr
            && self.asked
            && self
                .asked_at
                .is_some_and(|t| t.elapsed().as_millis() > after_ms)
        {
            self.asked_at = Some(Instant::now());
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(seq: u32, index: u16, shards: u16, len: u32, idr: bool) -> Header {
        Header {
            kind: KIND_VIDEO,
            flags: if idr { F_IDR } else { 0 },
            index,
            data_shards: shards,
            parity_shards: 0,
            seq,
            pts_us: seq.wrapping_mul(1000),
            len,
            host: 0,
        }
    }

    #[test]
    fn parses_the_header() {
        let mut b = vec![0u8; 24];
        b[0] = 1;
        b[1] = 1;
        b[2..4].copy_from_slice(&3u16.to_be_bytes());
        b[4..6].copy_from_slice(&5u16.to_be_bytes());
        b[8..12].copy_from_slice(&7u32.to_be_bytes());
        b[16..20].copy_from_slice(&6000u32.to_be_bytes());
        b.extend_from_slice(&[9, 9]);
        let (h, rest) = Header::parse(&b).unwrap();
        assert_eq!(
            (h.kind, h.flags, h.index, h.data_shards, h.seq, h.len),
            (1, 1, 3, 5, 7, 6000)
        );
        assert_eq!(rest, &[9, 9]);
        assert!(Header::parse(&b[..10]).is_none());
    }

    #[test]
    fn joins_shards_in_any_order_and_cuts_to_length() {
        let mut a = Assembler::new();
        let r = a.push(&hdr(1, 1, 2, 5, true), &[4, 5, 0, 0]);
        assert!(r.frame.is_none());
        let r = a.push(&hdr(1, 0, 2, 5, true), &[1, 2, 3]);
        assert_eq!(r.frame.unwrap().data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_loss_drops_everything_until_an_idr_asked_once() {
        let mut a = Assembler::new();
        assert!(a.push(&hdr(1, 0, 1, 1, true), &[1]).frame.is_some());
        // frame 2 loses its second shard, then a packet of frame 3 arrives
        assert!(a.push(&hdr(2, 0, 2, 2, false), &[2]).frame.is_none());
        assert!(a.push(&hdr(3, 0, 1, 1, false), &[3]).want_keyframe);
        assert_eq!(a.lost, 1);
        let r = a.push(&hdr(4, 0, 1, 1, false), &[4]);
        assert!(r.frame.is_none() && !r.want_keyframe, "asked once");
        assert!(
            a.push(&hdr(5, 0, 1, 1, true), &[5]).frame.is_some(),
            "an IDR resumes"
        );
        assert!(a.push(&hdr(6, 0, 1, 1, false), &[6]).frame.is_some());
    }

    #[test]
    fn old_frames_and_wrapping_counters() {
        let mut a = Assembler::new();
        assert!(a.push(&hdr(u32::MAX, 0, 1, 1, true), &[1]).frame.is_some());
        assert!(
            a.push(&hdr(0, 0, 1, 1, false), &[2]).frame.is_some(),
            "0 follows u32::MAX"
        );
        assert!(
            a.push(&hdr(u32::MAX, 0, 1, 1, true), &[1]).frame.is_none(),
            "repeat"
        );
    }

    /// A newer sender (PROTOCOL.md 4h) sets the playout delay in the last field and a timeline flag on its first
    /// frame. This receiver predates both: it must read and assemble those packets exactly as before.
    #[test]
    fn a_newer_senders_packets_read_as_before() {
        let mut b = vec![0u8; 24];
        b[0] = 1;
        b[1] = 1 | 8; // IDR, and the new timeline flag
        b[4..6].copy_from_slice(&1u16.to_be_bytes());
        b[16..20].copy_from_slice(&3u32.to_be_bytes());
        b[22..24].copy_from_slice(&1500u16.to_be_bytes()); // D = 150 ms
        b.extend_from_slice(&[7, 8, 9]);
        let (h, rest) = Header::parse(&b).unwrap();
        assert_eq!((h.kind, h.index, h.data_shards, h.len), (1, 0, 1, 3));
        let mut a = Assembler::new();
        let f = a.push(&h, rest).frame.expect("the frame is shown at once");
        assert!(f.idr);
        assert_eq!(f.data, vec![7, 8, 9]);
    }
}
