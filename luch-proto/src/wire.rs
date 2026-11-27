//! The media datagram of PROTOCOL.md section 4: the 24-byte header, cutting a frame into shards, joining shards back
//! into frames. Pure code without sockets, so every sender and receiver (Linux, Windows) shares one copy.

use std::time::Instant;

pub const HEADER: usize = 24;
/// Data bytes per datagram: HEADER + SHARD = 1400, safe under a 1500 MTU.
pub const SHARD: usize = 1376;

pub const KIND_VIDEO: u8 = 1;
pub const KIND_AUDIO: u8 = 2;
pub const F_IDR: u8 = 1;
pub const F_CONFIG: u8 = 2;
pub const F_PARITY: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
    /// Big-endian bytes; the reserved field stays 0.
    pub fn to_bytes(&self) -> [u8; HEADER] {
        let mut h = [0u8; HEADER];
        h[0] = self.kind;
        h[1] = self.flags;
        h[2..4].copy_from_slice(&self.index.to_be_bytes());
        h[4..6].copy_from_slice(&self.data_shards.to_be_bytes());
        h[6..8].copy_from_slice(&self.parity_shards.to_be_bytes());
        h[8..12].copy_from_slice(&self.seq.to_be_bytes());
        h[12..16].copy_from_slice(&self.pts_us.to_be_bytes());
        h[16..20].copy_from_slice(&self.len.to_be_bytes());
        h[20..22].copy_from_slice(&self.host.to_be_bytes());
        h
    }

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

/// The header as loose fields, in wire order (no FEC yet, so no parity shards).
#[allow(clippy::too_many_arguments)]
pub fn header(
    kind: u8,
    flags: u8,
    index: u16,
    data_shards: u16,
    seq: u32,
    pts_us: u32,
    len: u32,
    host: u16,
) -> [u8; HEADER] {
    Header {
        kind,
        flags,
        index,
        data_shards,
        parity_shards: 0,
        seq,
        pts_us,
        len,
        host,
    }
    .to_bytes()
}

/// One frame cut into datagram parts: a header for each shard and the shard itself. An empty frame still makes one
/// (empty) shard so the receiver sees its sequence number. The last shard is not padded; `len` cuts the frame.
pub fn shards<'a>(
    kind: u8,
    flags: u8,
    seq: u32,
    pts_us: u32,
    host: u16,
    data: &'a [u8],
) -> impl Iterator<Item = ([u8; HEADER], &'a [u8])> + 'a {
    let n = data.len().div_ceil(SHARD).max(1);
    (0..n).map(move |i| {
        let chunk = &data[(i * SHARD).min(data.len())..((i + 1) * SHARD).min(data.len())];
        let h = header(
            kind,
            flags,
            i as u16,
            n as u16,
            seq,
            pts_us,
            data.len() as u32,
            host,
        );
        (h, chunk)
    })
}

/// 19-byte OpusHead (RFC 7845) for 48 kHz stereo: the audio codec-config packet, MediaCodec CSD-0.
pub fn opus_head() -> [u8; 19] {
    let mut h = [0u8; 19];
    h[..8].copy_from_slice(b"OpusHead");
    h[8] = 1; // version
    h[9] = 2; // channels
    h[10..12].copy_from_slice(&312u16.to_le_bytes()); // pre-skip (opusenc lookahead)
    h[12..16].copy_from_slice(&48_000u32.to_le_bytes());
    h
}

pub struct Frame {
    pub seq: u32,
    pub pts_us: u32,
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

impl Default for Assembler {
    fn default() -> Self {
        Self::new()
    }
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
    fn header_layout() {
        let h = header(KIND_VIDEO, F_IDR, 2, 5, 0x01020304, 0x0a0b0c0d, 7000, 33);
        assert_eq!(&h[..8], &[1, 1, 0, 2, 0, 5, 0, 0]);
        assert_eq!(&h[8..12], &[1, 2, 3, 4]);
        assert_eq!(&h[12..16], &[0x0a, 0x0b, 0x0c, 0x0d]);
        assert_eq!(u32::from_be_bytes(h[16..20].try_into().unwrap()), 7000);
        assert_eq!(u16::from_be_bytes(h[20..22].try_into().unwrap()), 33);
        assert_eq!(&h[22..], &[0, 0], "reserved");
    }

    #[test]
    fn header_round_trips() {
        let h = Header {
            kind: KIND_AUDIO,
            flags: F_CONFIG | F_PARITY,
            index: 9,
            data_shards: 10,
            parity_shards: 2,
            seq: u32::MAX,
            pts_us: 123_456,
            len: 13_000,
            host: 57,
        };
        let mut b = h.to_bytes().to_vec();
        b.extend_from_slice(&[7, 8]);
        let (back, rest) = Header::parse(&b).unwrap();
        assert_eq!(back, h);
        assert_eq!(rest, &[7, 8]);
        assert!(Header::parse(&b[..HEADER - 1]).is_none());
    }

    #[test]
    fn opus_head_is_rfc7845() {
        let h = opus_head();
        assert_eq!(&h[..8], b"OpusHead");
        assert_eq!((h[8], h[9]), (1, 2));
        assert_eq!(u32::from_le_bytes(h[12..16].try_into().unwrap()), 48_000);
    }

    #[test]
    fn shards_cut_a_frame_and_the_assembler_joins_it() {
        let frame: Vec<u8> = (0..SHARD * 2 + 100).map(|i| i as u8).collect();
        let parts: Vec<_> = shards(KIND_VIDEO, F_IDR, 7, 1234, 5, &frame).collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[2].1.len(), 100, "the last shard is not padded");
        let mut a = Assembler::new();
        // Out of order, the way Wi-Fi sometimes delivers them.
        let mut out = None;
        for i in [2, 0, 1] {
            let mut datagram = parts[i].0.to_vec();
            datagram.extend_from_slice(parts[i].1);
            let (h, rest) = Header::parse(&datagram).unwrap();
            assert_eq!((h.index, h.data_shards, h.seq), (i as u16, 3, 7));
            out = a.push(&h, rest).frame.or(out);
        }
        let f = out.unwrap();
        assert_eq!((f.seq, f.pts_us, f.idr, f.host), (7, 1234, true, 5));
        assert_eq!(f.data, frame);
    }

    #[test]
    fn an_empty_frame_is_one_empty_shard() {
        let parts: Vec<_> = shards(KIND_AUDIO, 0, 1, 0, 0, &[]).collect();
        assert_eq!(parts.len(), 1);
        assert!(parts[0].1.is_empty());
        assert_eq!(u16::from_be_bytes([parts[0].0[4], parts[0].0[5]]), 1);
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
}
