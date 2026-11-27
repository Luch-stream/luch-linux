//! EncodedFrameBus: the single encoded stream, fanned out to per-receiver transports.

use tokio::sync::broadcast;

use super::frame::EncodedFrame;

/// Bounded channel capacity per kind; a receiver that falls a full channel behind is
/// "lagging" and recovers via IDR instead of an ever-growing queue.
pub const VIDEO_QUEUE: usize = 8;
pub const AUDIO_QUEUE: usize = 64;

/// Publisher of the group's encoded stream. One per MirrorGroup: the EncoderSession
/// publishes each frame once; every transport subscribes independently. Sending never
/// blocks on a slow receiver (tokio broadcast drops lagging receivers, it never back-pressures).
#[derive(Clone)]
pub struct FrameBus {
    video: broadcast::Sender<EncodedFrame>,
    audio: broadcast::Sender<EncodedFrame>,
}

impl Default for FrameBus {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameBus {
    pub fn new() -> Self {
        FrameBus {
            video: broadcast::channel(VIDEO_QUEUE).0,
            audio: broadcast::channel(AUDIO_QUEUE).0,
        }
    }

    /// Assign nothing here: the frame already carries its group-wide seq/pts.
    /// Never blocks; returns the subscriber count the frame reached.
    pub fn publish(&self, f: &EncodedFrame) -> usize {
        let bus = if f.is_video() {
            &self.video
        } else {
            &self.audio
        };
        bus.send(f.clone()).unwrap_or(0)
    }

    pub fn subscribe_video(&self) -> broadcast::Receiver<EncodedFrame> {
        self.video.subscribe()
    }

    pub fn subscribe_audio(&self) -> broadcast::Receiver<EncodedFrame> {
        self.audio.subscribe()
    }

    /// Subscriber counts for group stats.
    pub fn video_subscribers(&self) -> usize {
        self.video.receiver_count()
    }

    pub fn audio_subscribers(&self) -> usize {
        self.audio.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::frame::EncodedFrame;
    use super::super::{F_IDR, KIND_AUDIO, KIND_VIDEO};
    use super::*;

    fn video_frame(seq: u32) -> EncodedFrame {
        EncodedFrame::new(
            KIND_VIDEO,
            F_IDR,
            seq,
            seq * 1_000,
            10,
            Arc::from(vec![seq as u8; 8]),
        )
    }

    #[tokio::test]
    async fn subscribers_receive_identical_seq_pts_flags_and_shared_payload() {
        let bus = FrameBus::new();
        let mut a = bus.subscribe_video();
        let mut b = bus.subscribe_video();
        let f = video_frame(7);
        assert_eq!(bus.publish(&f), 2);
        let (fa, fb) = (a.recv().await.unwrap(), b.recv().await.unwrap());
        assert_eq!(fa.seq, fb.seq);
        assert_eq!(fa.pts_us, fb.pts_us);
        assert_eq!(fa.flags, fb.flags);
        assert!(Arc::ptr_eq(&fa.data, &fb.data));
    }

    #[tokio::test]
    async fn lagging_subscriber_is_dropped_not_blocking_the_fast_one() {
        let bus = FrameBus::new();
        let mut slow = bus.subscribe_video();
        let mut fast = bus.subscribe_video();
        for seq in 0..(VIDEO_QUEUE + 4) as u32 {
            bus.publish(&video_frame(seq));
        }
        // Both receivers may start lagged (all 12 frames landed before any recv).
        // Lagged errors are recovery signals; buffered newer frames follow. The fast
        // receiver eventually reaches the newest published frame (seq = VIDEO_QUEUE+3).
        let mut saw = None;
        for _ in 0..(VIDEO_QUEUE + 8) {
            match fast.recv().await {
                Ok(f) => saw = Some(f),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => panic!("bus closed"),
            }
            if saw
                .as_ref()
                .is_some_and(|f| f.seq == (VIDEO_QUEUE + 3) as u32)
            {
                break;
            }
        }
        assert_eq!(saw.unwrap().seq, (VIDEO_QUEUE + 3) as u32);
        assert!(slow.recv().await.is_err());
        // Publishing still works for the fast receiver.
        bus.publish(&video_frame(100));
        assert_eq!(fast.recv().await.unwrap().seq, 100);
    }

    #[tokio::test]
    async fn unsubscribing_one_receiver_keeps_the_bus_alive() {
        let bus = FrameBus::new();
        let mut a = bus.subscribe_video();
        let b = bus.subscribe_video();
        drop(b);
        bus.publish(&video_frame(1));
        assert_eq!(a.recv().await.unwrap().seq, 1);
        assert_eq!(bus.video_subscribers(), 1);
    }

    #[tokio::test]
    async fn audio_and_video_are_separate_channels() {
        let bus = FrameBus::new();
        let mut v = bus.subscribe_video();
        let mut a = bus.subscribe_audio();
        let vf = video_frame(3);
        let af = EncodedFrame::new(KIND_AUDIO, 0, 9, 48_000, 0, Arc::from(vec![0u8; 4]));
        bus.publish(&vf);
        bus.publish(&af);
        assert_eq!(v.recv().await.unwrap().kind, KIND_VIDEO);
        assert_eq!(a.recv().await.unwrap().kind, KIND_AUDIO);
        assert_eq!(bus.video_subscribers(), 1);
        assert_eq!(bus.audio_subscribers(), 1);
    }

    #[tokio::test]
    async fn dropping_the_bus_closes_subscriptions() {
        let bus = FrameBus::new();
        let mut rx = bus.subscribe_video();
        drop(bus);
        assert!(rx.recv().await.is_err());
    }
}
