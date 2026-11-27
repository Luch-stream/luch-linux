//! Common encoded frame: the unit that flows from EncoderSession to every receiver.

use std::sync::{Arc, OnceLock};

use bytes::Bytes;

use super::KIND_VIDEO;

/// One encoded media frame, shared by the whole group: the encoder publishes it once,
/// every receiver's transport reads the same payload through `Arc`.
#[derive(Clone, Debug)]
pub struct EncodedFrame {
    /// `KIND_VIDEO` / `KIND_AUDIO`.
    pub kind: u8,
    /// `F_IDR` | `F_CONFIG`, 0 for a regular frame.
    pub flags: u8,
    /// Group-wide sequence, assigned by the EncoderSession (per kind).
    pub seq: u32,
    /// Presentation timestamp in µs, shared media time base.
    pub pts_us: u32,
    /// Capture-to-encoded latency in 0.1 ms.
    pub host_lat: u16,
    /// Encoded payload; cloning the frame never copies these bytes.
    pub data: Arc<[u8]>,
    /// Websocket wire form (header + payload), built once per frame for all browsers.
    /// Behind an `Arc` because the bus clones the frame per subscriber: a plain OnceLock
    /// would be copied empty by `Clone`, serializing the same frame once per receiver.
    pub(crate) ws_wire: Arc<OnceLock<Bytes>>,
    /// UDP shards, built once per frame for all UDP receivers (shared with clones).
    pub(crate) udp_shards: Arc<OnceLock<Arc<[Bytes]>>>,
}

impl EncodedFrame {
    pub fn new(kind: u8, flags: u8, seq: u32, pts_us: u32, host_lat: u16, data: Arc<[u8]>) -> Self {
        EncodedFrame {
            kind,
            flags,
            seq,
            pts_us,
            host_lat,
            data,
            ws_wire: Arc::new(OnceLock::new()),
            udp_shards: Arc::new(OnceLock::new()),
        }
    }

    pub fn is_video(&self) -> bool {
        self.kind == KIND_VIDEO
    }

    pub fn is_keyframe(&self) -> bool {
        self.flags & super::F_IDR != 0
    }
}

/// `true` when sequence `a` is strictly newer than `b`, robust across the u32 wrap
/// (half the range on either side is "ahead": ~30 h of video at 60 fps, unbounded in practice).
pub fn seq_newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

#[cfg(test)]
mod tests {
    use super::super::{F_CONFIG, F_IDR, KIND_AUDIO};
    use super::*;

    #[test]
    fn frame_carries_media_metadata_without_copying_payload() {
        let data: Arc<[u8]> = vec![1u8, 2, 3].into();
        let f = EncodedFrame::new(KIND_VIDEO, F_IDR, 42, 123_456, 33, data.clone());
        assert!(f.is_video());
        assert!(f.is_keyframe());
        assert_eq!(f.seq, 42);
        assert_eq!(f.pts_us, 123_456);
        assert_eq!(f.host_lat, 33);
        // Cloning shares the allocation, it must not copy the payload.
        let g = f.clone();
        assert!(Arc::ptr_eq(&g.data, &f.data));
        assert_eq!(&*g.data, &[1, 2, 3]);
    }

    #[test]
    fn audio_frame_flags_and_kind() {
        let f = EncodedFrame::new(KIND_AUDIO, F_CONFIG, 0, 96_000, 0, vec![7u8; 4].into());
        assert!(!f.is_video());
        assert!(!f.is_keyframe());
        assert!(f.flags & F_CONFIG != 0);
    }

    #[test]
    fn seq_newer_orders_numbers_across_wraparound() {
        assert!(seq_newer(1, 0));
        assert!(!seq_newer(0, 1));
        assert!(seq_newer(0, u32::MAX)); // wrap: 0 follows MAX
        assert!(!seq_newer(u32::MAX, 0));
        assert!(!seq_newer(5, 5)); // equal is not newer
        assert!(!seq_newer(0, u32::MAX / 2 + 1)); // more than half-range behind
    }
}
