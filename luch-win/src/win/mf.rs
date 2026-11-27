//! The hardware H.264/HEVC encoder through Media Foundation: the one encoder API every vendor ships with its driver
//! (NVIDIA, AMD, Intel, Qualcomm), taking D3D11 textures without a copy to the CPU. It is the first encoder because it
//! needs no SDK; NVENC, AMF and oneVPL directly come next (docs/windows.md, "Encoders"), since their MFTs add a
//! little latency and lack some knobs (no slice or VBV control on some drivers).
//!
//! Hardware MFTs are asynchronous: they say when they want input (METransformNeedInput) and when output is ready
//! (METransformHaveOutput). One picture in, its packet out, before the next goes in: one frame in flight.

use std::mem::ManuallyDrop;

use windows::core::{Interface, GUID};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::VARIANT;

use super::convert::Converter;
use crate::pipeline::{Encoder, Packet};
use crate::settings::LowLatency;

pub struct MfEncoder {
    mft: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: ICodecAPI,
    conv: Converter,
    /// METransformNeedInput events not answered yet.
    credits: u32,
    frame: i64,
    frame_100ns: i64,
    name: String,
    _manager: IMFDXGIDeviceManager,
}

/// MF_MT_FRAME_SIZE and MF_MT_FRAME_RATE pack two u32 into one u64, high first.
fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn set(codec: &ICodecAPI, api: &GUID, v: VARIANT) -> windows::core::Result<()> {
    // SAFETY: valid GUID and VARIANT for the call.
    unsafe { codec.SetValue(api, &v) }
}

/// The first hardware encoder MFT for `subtype` that takes NV12, best first (SORTANDFILTER puts the GPU's own one,
/// by merit, ahead of others).
fn hardware_mft(subtype: GUID) -> windows::core::Result<(IMFTransform, String)> {
    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype,
    };
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut n = 0u32;
    // SAFETY: MFTEnumEx fills a CoTaskMem array of n activates; each is taken out and the array freed.
    unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut list,
            &mut n,
        )?;
        let all: Vec<Option<IMFActivate>> =
            (0..n as usize).map(|i| (*list.add(i)).take()).collect();
        CoTaskMemFree(Some(list as *const _));
        let first = all.into_iter().flatten().next().ok_or_else(|| {
            windows::core::Error::new(MF_E_TOPO_CODEC_NOT_FOUND, "no hardware encoder")
        })?;
        let mut name = [0u16; 128];
        let mut len = 0u32;
        let _ = first.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut name, Some(&mut len));
        let name = String::from_utf16_lossy(&name[..len as usize]);
        Ok((first.ActivateObject()?, name))
    }
}

impl MfEncoder {
    pub fn new(
        dev: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        capture: (u32, u32),
        s: &LowLatency,
    ) -> windows::core::Result<Self> {
        // SAFETY: COM and MF calls on valid interfaces; attribute GUIDs are constants.
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
            let subtype = if s.hevc {
                MFVideoFormat_HEVC
            } else {
                MFVideoFormat_H264
            };
            let (mft, name) = hardware_mft(subtype)?;
            // Hardware MFTs refuse every call until the caller says it handles their events itself.
            mft.GetAttributes()?
                .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;

            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager: IMFDXGIDeviceManager = manager.unwrap();
            manager.ResetDevice(dev, token)?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

            let video = |sub: &GUID| -> windows::core::Result<IMFMediaType> {
                let t = MFCreateMediaType()?;
                t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                t.SetGUID(&MF_MT_SUBTYPE, sub)?;
                t.SetUINT64(&MF_MT_FRAME_SIZE, pack(s.width, s.height))?;
                t.SetUINT64(&MF_MT_FRAME_RATE, pack(s.fps, 1))?;
                t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                Ok(t)
            };
            // The output type goes first: encoders list their input types only once they know it.
            let out = video(&subtype)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, s.kbps * 1000)?;
            mft.SetOutputType(0, &out, 0)?;
            mft.SetInputType(0, &video(&MFVideoFormat_NV12)?, 0)?;

            let codec: ICodecAPI = mft.cast()?;
            // Low-latency mode: no reordering, output per input (the MFT's own "no B-frames, no lookahead").
            set(&codec, &CODECAPI_AVLowLatencyMode, true.into())?;
            set(
                &codec,
                &CODECAPI_AVEncCommonRateControlMode,
                (eAVEncCommonRateControlMode_CBR.0 as u32).into(),
            )?;
            set(
                &codec,
                &CODECAPI_AVEncCommonMeanBitRate,
                (s.kbps * 1000).into(),
            )?;
            set(&codec, &CODECAPI_AVEncMPVDefaultBPictureCount, 0u32.into())?;
            // GOP 0: no automatic IDR, only on request.
            let _ = set(&codec, &CODECAPI_AVEncMPVGOPSize, 0u32.into());
            // Not every driver takes these; the session works without them, a bit larger frames.
            let _ = set(&codec, &CODECAPI_AVEncCommonBufferSize, s.vbv_bits().into());
            let _ = set(&codec, &CODECAPI_AVEncVideoMaxNumRefFrame, 1u32.into());
            let _ = set(&codec, &CODECAPI_AVEncCommonQualityVsSpeed, 0u32.into());

            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let events: IMFMediaEventGenerator = mft.cast()?;
            let conv = Converter::new(dev, ctx, capture, (s.width, s.height), s.fps)?;
            Ok(MfEncoder {
                mft,
                events,
                codec,
                conv,
                credits: 0,
                frame: 0,
                frame_100ns: 10_000_000 / s.fps.max(1) as i64,
                name,
                _manager: manager,
            })
        }
    }

    /// Waits for the MFT's next event and returns its type.
    fn event(&self) -> windows::core::Result<u32> {
        // SAFETY: blocking GetEvent on a valid generator.
        unsafe {
            self.events
                .GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0))?
                .GetType()
        }
    }

    fn take_output(&mut self) -> windows::core::Result<Packet> {
        let mut out = [MFT_OUTPUT_DATA_BUFFER::default()];
        let mut status = 0u32;
        // SAFETY: the MFT provides its own output sample (hardware encoders do); both COM fields of the buffer are
        // dropped by hand.
        unsafe {
            let r = self.mft.ProcessOutput(0, &mut out, &mut status);
            let [mut b] = out;
            let sample = ManuallyDrop::take(&mut b.pSample);
            ManuallyDrop::drop(&mut b.pEvents);
            r?;
            let sample = sample
                .ok_or_else(|| windows::core::Error::new(MF_E_UNEXPECTED, "no output sample"))?;
            let idr = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
            let buf = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buf.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buf.Unlock()?;
            Ok(Packet { data, idr })
        }
    }
}

impl Encoder for MfEncoder {
    type Frame = ID3D11Texture2D;

    fn name(&self) -> &str {
        &self.name
    }

    fn encode(&mut self, frame: &ID3D11Texture2D, idr: bool) -> Result<Packet, String> {
        let e = |what: &'static str| move |err: windows::core::Error| format!("{what}: {err}");
        while self.credits == 0 {
            match self.event().map_err(e("GetEvent"))? as i32 {
                t if t == METransformNeedInput.0 => self.credits += 1,
                t if t == METransformHaveOutput.0 => {
                    let _ = self.take_output(); // a late packet of an earlier frame: no longer wanted
                }
                _ => {}
            }
        }
        self.conv.convert(frame).map_err(e("VideoProcessorBlt"))?;
        // SAFETY: MF calls on valid interfaces; the sample holds its own reference to the texture.
        unsafe {
            if idr {
                let _ = set(&self.codec, &CODECAPI_AVEncVideoForceKeyFrame, 1u32.into());
            }
            let buf = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &self.conv.nv12, 0, false)
                .map_err(e("MFCreateDXGISurfaceBuffer"))?;
            let sample = MFCreateSample().map_err(e("MFCreateSample"))?;
            sample.AddBuffer(&buf).map_err(e("AddBuffer"))?;
            sample
                .SetSampleTime(self.frame * self.frame_100ns)
                .map_err(e("SetSampleTime"))?;
            sample
                .SetSampleDuration(self.frame_100ns)
                .map_err(e("SetSampleDuration"))?;
            self.frame += 1;
            self.mft
                .ProcessInput(0, &sample, 0)
                .map_err(e("ProcessInput"))?;
        }
        self.credits -= 1;
        loop {
            match self.event().map_err(e("GetEvent"))? as i32 {
                t if t == METransformNeedInput.0 => self.credits += 1,
                t if t == METransformHaveOutput.0 => {
                    let mut p = self.take_output().map_err(e("ProcessOutput"))?;
                    p.idr |= idr;
                    return Ok(p);
                }
                _ => {}
            }
        }
    }
}

impl Drop for MfEncoder {
    fn drop(&mut self) {
        // SAFETY: valid MFT; shutdown after the last use.
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = MFShutdown();
        }
    }
}
