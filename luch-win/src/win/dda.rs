//! DXGI Desktop Duplication: the monitor's picture as a D3D11 texture on the GPU that drives the monitor, stamped
//! with the QPC time of the flip. Sunshine's first choice (display_base.cpp:1030); docs/windows.md compares it with
//! Windows.Graphics.Capture.

use std::time::Duration;

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8X8_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutput5, IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_DENIED,
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};

use super::sys::{follow_input_desktop, Qpc};
use crate::pipeline::{Capture, CaptureError, Captured};
use crate::settings::Vendor;

/// The adapter and output for `index`, counting attached outputs over all adapters. On a hybrid laptop the internal
/// panel hangs off the iGPU and an external port often off the dGPU: the device must be made on the adapter that
/// owns the output, or DuplicateOutput fails (Sunshine display_base.cpp:449-527).
pub fn find_output(index: u32) -> windows::core::Result<(IDXGIAdapter1, IDXGIOutput, Vendor)> {
    // SAFETY: COM calls on valid interfaces; out-structs are owned values.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut seen = 0;
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            let vendor = Vendor::from_pci(adapter.GetDesc1()?.VendorId);
            let mut o = 0;
            while let Ok(output) = adapter.EnumOutputs(o) {
                if output.GetDesc()?.AttachedToDesktop.as_bool() {
                    if seen == index {
                        return Ok((adapter, output, vendor));
                    }
                    seen += 1;
                }
                o += 1;
            }
            a += 1;
        }
    }
    Err(windows::core::Error::new(
        DXGI_ERROR_ACCESS_LOST,
        "no such monitor",
    ))
}

/// A D3D11 device for capture, conversion and encoding at once: one thread uses it, so no keyed mutex or second
/// device is needed (Sunshine uses two devices and a keyed mutex because its capture and encode threads differ,
/// display_vram.cpp:1968-2006). Video support for the video processor and the encoder; GPU thread priority 7, the
/// highest, as Sunshine (display_base.cpp:718).
pub fn device(
    adapter: &IDXGIAdapter1,
) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut dev = None;
    let mut ctx = None;
    // SAFETY: out-pointers to locals; the adapter is valid.
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut dev),
            None,
            Some(&mut ctx),
        )?;
        let dev: ID3D11Device = dev.unwrap();
        // Media Foundation calls the device from its own threads.
        let _ = dev
            .cast::<ID3D11Multithread>()?
            .SetMultithreadProtected(true);
        let _ = dev.cast::<IDXGIDevice>()?.SetGPUThreadPriority(7);
        Ok((dev, ctx.unwrap()))
    }
}

/// Pictures kept by us: the duplication's own texture must be released before the next AcquireNextFrame, and a
/// still screen is encoded again from the last copy. Two, so the copy never writes the texture being encoded.
const POOL: usize = 2;

pub struct Dda {
    dup: IDXGIOutputDuplication,
    ctx: ID3D11DeviceContext,
    pool: Vec<ID3D11Texture2D>,
    next: usize,
    size: (u32, u32),
    qpc: Qpc,
    held: bool,
}

impl Dda {
    pub fn open(
        dev: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        output: &IDXGIOutput,
    ) -> Result<Self, String> {
        let e = |what: &'static str| move |err: windows::core::Error| format!("{what}: {err}");
        follow_input_desktop();
        // Formats we can convert: 8-bit only for now. HDR would add R16G16B16A16_FLOAT first, as Sunshine does
        // (display_vram.cpp:2025-2046), and a P010 path.
        let formats: [DXGI_FORMAT; 3] = [
            DXGI_FORMAT_B8G8R8A8_UNORM,
            DXGI_FORMAT_B8G8R8X8_UNORM,
            DXGI_FORMAT_R8G8B8A8_UNORM,
        ];
        // SAFETY: COM calls on valid interfaces.
        let dup = unsafe {
            match output.cast::<IDXGIOutput5>() {
                // DuplicateOutput1 gives the picture in a format we listed, without a conversion by DWM.
                Ok(o5) => o5.DuplicateOutput1(dev, 0, &formats),
                Err(_) => output
                    .cast::<IDXGIOutput1>()
                    .and_then(|o1| o1.DuplicateOutput(dev)),
            }
        }
        .map_err(e("DuplicateOutput"))?;
        // SAFETY: plain struct out-value.
        let desc = unsafe { dup.GetDesc() };
        let size = (desc.ModeDesc.Width, desc.ModeDesc.Height);
        let tex = D3D11_TEXTURE2D_DESC {
            Width: size.0,
            Height: size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: desc.ModeDesc.Format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut pool = Vec::with_capacity(POOL);
        for _ in 0..POOL {
            let mut t = None;
            // SAFETY: desc and out-pointer are valid.
            unsafe { dev.CreateTexture2D(&tex, None, Some(&mut t)) }
                .map_err(e("CreateTexture2D"))?;
            pool.push(t.unwrap());
        }
        Ok(Dda {
            dup,
            ctx: ctx.clone(),
            pool,
            next: 0,
            size,
            qpc: Qpc::new(),
            held: false,
        })
    }
}

impl Capture for Dda {
    type Frame = ID3D11Texture2D;

    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn next(&mut self, timeout: Duration) -> Result<Captured<ID3D11Texture2D>, CaptureError> {
        // SAFETY: COM calls on valid interfaces; the frame is released before the next acquire.
        unsafe {
            if self.held {
                let _ = self.dup.ReleaseFrame();
                self.held = false;
            }
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            match self
                .dup
                .AcquireNextFrame(timeout.as_millis() as u32, &mut info, &mut res)
            {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Err(CaptureError::Timeout),
                Err(e)
                    if e.code() == DXGI_ERROR_ACCESS_LOST
                        || e.code() == DXGI_ERROR_ACCESS_DENIED =>
                {
                    return Err(CaptureError::Reinit(e.to_string()))
                }
                Err(e) => return Err(CaptureError::Reinit(format!("AcquireNextFrame: {e}"))),
            }
            self.held = true;
            // Only the pointer moved: nothing new in the picture. The cursor is not in DDA's picture at all; it is
            // drawn by us (later: Sunshine's shader blend, display_vram.cpp:1663-1693) or comes with WGC.
            if info.LastPresentTime == 0 {
                return Err(CaptureError::Timeout);
            }
            let src: ID3D11Texture2D = res
                .ok_or_else(|| CaptureError::Reinit("no desktop texture".into()))?
                .cast()
                .map_err(|e| CaptureError::Fatal(e.to_string()))?;
            let dst = self.pool[self.next].clone();
            self.next = (self.next + 1) % POOL;
            self.ctx.CopyResource(&dst, &src);
            Ok(Captured {
                frame: dst,
                captured_us: self.qpc.ticks_to_us(info.LastPresentTime),
            })
        }
    }
}

impl Drop for Dda {
    fn drop(&mut self) {
        if self.held {
            // SAFETY: a frame is held.
            let _ = unsafe { self.dup.ReleaseFrame() };
        }
    }
}
