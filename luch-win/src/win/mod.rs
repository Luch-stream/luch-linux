//! The Windows half: Desktop Duplication in, the D3D11 video processor to NV12, the Media Foundation hardware
//! encoder, all on the GPU that drives the monitor; then the shared pipeline, control channel and sender.

pub mod audio;
pub mod convert;
pub mod dda;
pub mod mf;
pub mod shell;
pub mod sys;
pub mod wgc;

use std::net::SocketAddrV4;
use std::sync::Arc;

use luch_proto::{control, session, wire};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::cli::Options;
use crate::pipeline::{pump, Encoder, Signals};
use crate::settings::LowLatency;
use crate::tx::{open_udp, Tx};

/// The sender's id, made once and kept (PROTOCOL.md section 1), in %APPDATA%\\Luch.
pub fn sender_id() -> String {
    let file = std::env::var_os("APPDATA")
        .map(|d| std::path::PathBuf::from(d).join("Luch").join("sender-id"));
    if let Some(id) = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()) {
        if uuid::Uuid::parse_str(id.trim()).is_ok() {
            return id.trim().to_string();
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Some(f) = file {
        let _ = f.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(f, &id);
    }
    id
}

pub fn host_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into())
}

pub fn run(o: &Options) -> Result<(), String> {
    // SAFETY: once per thread, before any COM call.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(|e| format!("CoInitializeEx: {e}"))?;
    let _session = sys::SessionGuard::start();
    let _mmcss = sys::Mmcss::capture();

    let (adapter, output, vendor) =
        dda::find_output(o.output).map_err(|e| format!("monitor {}: {e}", o.output))?;
    let (dev, ctx) = dda::device(&adapter).map_err(|e| format!("D3D11CreateDevice: {e}"))?;
    let first = dda::Dda::open(&dev, &ctx, &output)?;
    let screen = crate::pipeline::Capture::size(&first);
    let mut first = Some(first);

    let id = sender_id();
    let mut ch = crate::control::Channel::open(o.to, o.from, &id, &host_name(), &["mirror"])
        .map_err(|e| format!("control channel: {e}"))?;
    let codec = session::choose_codec(&ch.welcome, &o.codec);
    let codec = if codec == "video/hevc" {
        "video/hevc"
    } else {
        "video/avc"
    };
    let (w, h) = session::session_size(o.size.as_deref(), ch.welcome["max"].as_str());
    let video = control::Video {
        codec,
        width: w,
        height: h,
        fps: session::FPS,
    };
    let settings = LowLatency::new(w, h, session::FPS, codec == "video/hevc");
    let mut enc =
        mf::MfEncoder::new(&dev, &ctx, screen, &settings).map_err(|e| format!("encoder: {e}"))?;
    eprintln!(
        "[luch-win] {}x{} -> {w}x{h} {codec} with {} ({vendor:?})",
        screen.0,
        screen.1,
        enc.name()
    );
    let (vport, _aport) = ch.start(&video).map_err(|e| format!("start: {e}"))?;
    let signals = Arc::new(Signals::default());
    let _ctl = ch.follow(signals.clone()).map_err(|e| e.to_string())?;

    let sock =
        open_udp(o.from, SocketAddrV4::new(*o.to.ip(), vport)).map_err(|e| format!("udp: {e}"))?;
    let qos = sys::Qos::new();
    if let Some(q) = &qos {
        q.add(&sock, false);
    }
    let mut video_tx = Tx::new(sock, wire::KIND_VIDEO);
    let clock = sys::Qpc::new();
    let mut base = None;
    let open = || match first.take() {
        Some(c) => Ok(c),
        None => dda::Dda::open(&dev, &ctx, &output),
    };
    let counts = pump(
        open,
        &mut enc,
        &clock,
        session::FPS,
        &signals,
        |p, at, host| {
            let base = *base.get_or_insert(at);
            let flags = if p.idr { wire::F_IDR } else { 0 };
            video_tx.send_frame(&p.data, flags, at - base, host);
        },
    )?;
    eprintln!("[luch-win] done: {counts:?}");
    Ok(())
}
