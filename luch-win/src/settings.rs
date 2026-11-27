//! Encoder settings for the lowest latency, one place for every vendor. The values are what Sunshine and ALVR ship
//! (docs/windows.md, "Encoders"), held to the Luch rules: no B-frames, one reference, an IDR only when asked for,
//! parameter sets in every IDR, a VBV of one frame so no frame waits behind a large one.

use luch_proto::session::kbps_for;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
    /// Anything else with a Media Foundation hardware encoder (Qualcomm on ARM laptops).
    Other,
}

impl Vendor {
    /// From DXGI_ADAPTER_DESC1::VendorId.
    pub fn from_pci(id: u32) -> Self {
        match id {
            0x10de => Vendor::Nvidia,
            0x1002 | 0x1022 => Vendor::Amd,
            0x8086 => Vendor::Intel,
            _ => Vendor::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LowLatency {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub kbps: u32,
    pub hevc: bool,
}

impl LowLatency {
    pub fn new(width: u32, height: u32, fps: u32, hevc: bool) -> Self {
        LowLatency {
            width,
            height,
            fps,
            kbps: kbps_for(width, height),
            hevc,
        }
    }

    /// The rate-control buffer: one frame's worth of bits at the target rate (Sunshine nvenc_base.cpp:276-280, ALVR
    /// VideoEncoderNVENC.cpp:390-393 adds 10%). Larger lets a frame grow and the next ones queue behind it on the wire.
    pub fn vbv_bits(&self) -> u32 {
        (self.kbps as u64 * 1000 / self.fps.max(1) as u64) as u32
    }

    /// The FFmpeg encoder and its options for a vendor, the way luch-kms drives h264_vaapi: a D3D11 frames context
    /// in, packets out. Codec-independent fields (no B-frames, infinite GOP, CBR, VBV) are set on AVCodecContext by
    /// the caller: max_b_frames 0, gop_size and keyint_min INT_MAX, AV_CODEC_FLAG_LOW_DELAY|CLOSED_GOP,
    /// rc_max_rate = bit_rate, rc_buffer_size = vbv_bits().
    pub fn ffmpeg(&self, vendor: Vendor) -> (&'static str, Vec<(&'static str, &'static str)>) {
        let codec = |h264, hevc| if self.hevc { hevc } else { h264 };
        match vendor {
            // Sunshine drives NVENC through the SDK (src/nvenc); through FFmpeg these are the same knobs.
            Vendor::Nvidia => (
                codec("h264_nvenc", "hevc_nvenc"),
                vec![
                    ("preset", "p1"),
                    ("tune", "ull"),
                    ("rc", "cbr"),
                    ("zerolatency", "1"),
                    ("delay", "0"),
                    ("forced-idr", "1"),
                    ("rc-lookahead", "0"),
                    ("multipass", "disabled"),
                ],
            ),
            // Sunshine src/video.cpp:1112-1223 and config.cpp:742-751.
            Vendor::Amd => (
                codec("h264_amf", "hevc_amf"),
                vec![
                    ("usage", "ultralowlatency"),
                    ("rc", "cbr"),
                    ("quality", "speed"),
                    ("latency", "1"),
                    ("async_depth", "1"),
                    ("filler_data", "0"),
                    ("forced_idr", "1"),
                    ("frame_skipping", "0"),
                    ("preencode", "0"),
                    ("header_insertion_mode", "idr"),
                ],
            ),
            // Sunshine src/video.cpp:977-1107.
            Vendor::Intel => (
                codec("h264_qsv", "hevc_qsv"),
                vec![
                    ("preset", "veryfast"),
                    ("async_depth", "1"),
                    ("low_delay_brc", "1"),
                    ("low_power", "1"),
                    ("forced_idr", "1"),
                    ("look_ahead", "0"),
                ],
            ),
            // Sunshine src/video.cpp:1228-1288: Media Foundation has no IDR on request through FFmpeg, see
            // `win::mf` for the direct MFT with CODECAPI_AVEncVideoForceKeyFrame.
            Vendor::Other => (
                codec("h264_mf", "hevc_mf"),
                vec![
                    ("hw_encoding", "1"),
                    ("rate_control", "cbr"),
                    ("scenario", "display_remoting"),
                ],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_holds_one_frame() {
        let s = LowLatency::new(1920, 1080, 60, false);
        assert_eq!(s.kbps, 20_000);
        assert_eq!(s.vbv_bits(), 333_333);
        assert_eq!(LowLatency::new(3840, 2160, 60, true).vbv_bits(), 666_666);
    }

    #[test]
    fn vendors_from_pci_ids() {
        assert_eq!(Vendor::from_pci(0x10de), Vendor::Nvidia);
        assert_eq!(Vendor::from_pci(0x1002), Vendor::Amd);
        assert_eq!(Vendor::from_pci(0x8086), Vendor::Intel);
        assert_eq!(Vendor::from_pci(0x5143), Vendor::Other);
    }

    #[test]
    fn every_vendor_has_its_encoder_with_one_frame_in_flight() {
        for hevc in [false, true] {
            let s = LowLatency::new(1920, 1080, 60, hevc);
            for v in [Vendor::Nvidia, Vendor::Amd, Vendor::Intel, Vendor::Other] {
                let (name, opts) = s.ffmpeg(v);
                assert_eq!(name.starts_with("hevc_"), hevc, "{name}");
                let get = |k| opts.iter().find(|(o, _)| *o == k).map(|(_, v)| *v);
                match v {
                    Vendor::Nvidia => {
                        assert_eq!(get("tune"), Some("ull"));
                        assert_eq!(get("delay"), Some("0"));
                    }
                    Vendor::Amd => assert_eq!(get("usage"), Some("ultralowlatency")),
                    Vendor::Intel => assert_eq!(get("low_delay_brc"), Some("1")),
                    Vendor::Other => assert_eq!(get("scenario"), Some("display_remoting")),
                }
                if v == Vendor::Amd || v == Vendor::Intel {
                    assert_eq!(get("async_depth"), Some("1"), "{name}");
                }
            }
        }
    }
}
