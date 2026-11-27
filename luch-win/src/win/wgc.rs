//! Windows.Graphics.Capture: the second capture, for what Desktop Duplication cannot do (docs/windows.md, "DDA or
//! WGC"): one window instead of a monitor, a cursor drawn by the system, outputs DDA refuses on some hybrid laptops.
//! It costs a frame pool and a yellow border on Windows 10 before 2104 (IsBorderRequired), and is capped at 60 fps
//! unless MinUpdateInterval is settable (Windows 11 24H2; Sunshine display_wgc.cpp:150-165).
//!
//! Not written yet; the plan, after Sunshine display_wgc.cpp:103-198:
//! `CreateDirect3D11DeviceFromDXGIDevice` on our device, `GraphicsCaptureItem` for the monitor through
//! `IGraphicsCaptureItemInterop::CreateForMonitor`, `Direct3D11CaptureFramePool::CreateFreeThreaded` with two buffers
//! (B8G8R8A8), `FrameArrived` keeping only the newest frame and waking `next()`, `SystemRelativeTime` as the capture
//! time (QPC, the same clock as DDA), `IsBorderRequired(false)` and `MinUpdateInterval(1 ms)` where present.

use windows::Graphics::Capture::GraphicsCaptureSession;

/// Whether this Windows has the API at all (Windows 10 1803+).
pub fn supported() -> bool {
    GraphicsCaptureSession::IsSupported().unwrap_or(false)
}
