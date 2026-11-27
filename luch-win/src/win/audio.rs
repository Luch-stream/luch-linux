//! What the PC plays, from WASAPI loopback on the default output: shared mode, event driven, the engine converting
//! to 48 kHz float stereo, as Sunshine opens it (src/platform/windows/audio.cpp:395-403). Opus encoding (5 ms frames,
//! RESTRICTED_LOWDELAY, src/audio.cpp:121-138) and sending are the next step; this gives the PCM.
//!
//! Loopback hears the device only while something plays; silence comes as no packets at all, so the caller fills
//! gaps by the clock. The local speakers keep playing: Sunshine switches the default device to a virtual sink
//! instead (audio.cpp:1131), which needs one installed.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use super::sys::Mmcss;

pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;

fn format() -> WAVEFORMATEXTENSIBLE {
    let block = CHANNELS * 4;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: CHANNELS,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * block as u32,
            nBlockAlign: block,
            wBitsPerSample: 32,
            cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>()
                - std::mem::size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: 0x3, // front left, front right
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    }
}

/// Calls `f` with interleaved float stereo and the QPC time (100 ns units) of its first sample, until `stop`.
/// Runs on the calling thread, which joins MMCSS "Pro Audio" for the time.
pub fn loopback(stop: &AtomicBool, mut f: impl FnMut(&[f32], u64)) -> windows::core::Result<()> {
    let _mmcss = Mmcss::pro_audio();
    // SAFETY: COM must be initialised on this thread (CoInitializeEx in `win::run`); every pointer passed is valid
    // for its call, and the buffer from GetBuffer is read before ReleaseBuffer.
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let fmt = format();
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            0,
            0,
            &fmt.Format,
            None,
        )?;
        let event = CreateEventW(None, false, false, None)?;
        client.SetEventHandle(event)?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let mut pcm = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            // The engine's period is 10 ms; 20 ms lets a stop through without spinning.
            if WaitForSingleObject(event, 20) != WAIT_OBJECT_0 {
                continue;
            }
            while capture.GetNextPacketSize()? > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                let mut qpc = 0u64;
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
                let n = frames as usize * CHANNELS as usize;
                pcm.clear();
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    pcm.resize(n, 0.0);
                } else {
                    pcm.extend_from_slice(std::slice::from_raw_parts(data as *const f32, n));
                }
                capture.ReleaseBuffer(frames)?;
                f(&pcm, qpc);
            }
        }
        let _ = client.Stop();
        let _ = CloseHandle(event);
    }
    Ok(())
}
