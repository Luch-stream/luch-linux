//! Safe wrapper over enc.c: DMA-BUF NV12 -> h264_vaapi with Sunshine-style low-latency settings.

use std::ffi::{c_char, c_int, CStr, CString};
use std::os::fd::RawFd;

#[repr(C)]
struct Raw {
    _p: [u8; 0],
}

extern "C" {
    fn luch_enc_open(
        node: *const c_char,
        w: c_int,
        h: c_int,
        fps: c_int,
        kbps: c_int,
        err: *mut c_char,
        n: usize,
    ) -> *mut Raw;
    fn luch_enc_encode(
        e: *mut Raw,
        fd: c_int,
        modifier: u64,
        off0: u32,
        pitch0: u32,
        off1: u32,
        pitch1: u32,
        pts_us: i64,
        force_idr: c_int,
        out: *mut *const u8,
        is_key: *mut c_int,
        err: *mut c_char,
        n: usize,
    ) -> c_int;
    fn luch_enc_close(e: *mut Raw);
}

/// One NV12 DMA-BUF frame: a single fd holding both planes.
pub struct Nv12Dmabuf {
    pub fd: RawFd,
    pub modifier: u64,
    pub offsets: [u32; 2],
    pub pitches: [u32; 2],
}

pub struct Encoder(*mut Raw);

// The C encoder is used from one thread at a time (callers hold a Mutex).
unsafe impl Send for Encoder {}

fn msg(buf: &[c_char]) -> String {
    // SAFETY: enc.c always NUL-terminates via snprintf, and the buffer was zeroed.
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

impl Encoder {
    pub fn open(render_node: &str, w: u32, h: u32, fps: u32, kbps: u32) -> Result<Self, String> {
        let node = CString::new(render_node).map_err(|e| e.to_string())?;
        let mut err = [0 as c_char; 256];
        // SAFETY: valid C string and error buffer; returns NULL on failure.
        let p = unsafe {
            luch_enc_open(
                node.as_ptr(),
                w as c_int,
                h as c_int,
                fps as c_int,
                kbps as c_int,
                err.as_mut_ptr(),
                err.len(),
            )
        };
        if p.is_null() {
            return Err(format!("encoder: {}", msg(&err)));
        }
        Ok(Encoder(p))
    }

    /// Encodes a frame; calls `out(bytes, is_idr)` with the packet before returning.
    pub fn encode(
        &mut self,
        f: &Nv12Dmabuf,
        pts_us: i64,
        force_idr: bool,
        out: impl FnOnce(&[u8], bool),
    ) -> Result<(), String> {
        let mut err = [0 as c_char; 256];
        let (mut data, mut key) = (std::ptr::null(), 0);
        // SAFETY: self.0 is a live encoder; the fd stays valid for the call (caller holds the buffer).
        let n = unsafe {
            luch_enc_encode(
                self.0,
                f.fd,
                f.modifier,
                f.offsets[0],
                f.pitches[0],
                f.offsets[1],
                f.pitches[1],
                pts_us,
                force_idr as c_int,
                &mut data,
                &mut key,
                err.as_mut_ptr(),
                err.len(),
            )
        };
        if n < 0 {
            return Err(format!("encode: {}", msg(&err)));
        }
        if n > 0 {
            // SAFETY: enc.c guarantees `data` points at `n` bytes until the next encode call.
            out(
                unsafe { std::slice::from_raw_parts(data, n as usize) },
                key != 0,
            );
        }
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: self.0 came from luch_enc_open and is freed exactly once.
        unsafe { luch_enc_close(self.0) }
    }
}
