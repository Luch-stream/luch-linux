//! luch-kms: see src/kms.c. Kept as a separate, minimal binary so only it needs CAP_SYS_ADMIN.
//!
//! `luch-kms --serve SOCKET [--uid N]` runs it as a small root service instead (for a sandboxed luchd, e.g. in
//! Flatpak, where file capabilities do not work): each connection to SOCKET sends one line of options
//! (`--size WxH --fps N --kbps N --codec h264|hevc`), then gets the same stream and control a child would
//! (packets out, `k` in, end of connection quits). The socket belongs to `--uid` and is mode 0600.
use std::ffi::{c_char, c_int, CString};
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};

extern "C" {
    fn luch_kms_main(argc: c_int, argv: *const *const c_char) -> c_int;
    fn chown(path: *const c_char, uid: u32, gid: u32) -> c_int;
}

/// One option line from a client: only the options the grabber takes, with plain values.
fn options(line: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut it = line.split_whitespace();
    while let Some(k) = it.next() {
        let v = it.next()?;
        let ok = match k {
            "--size" => v
                .split_once('x')
                .is_some_and(|(w, h)| w.parse::<u32>().is_ok() && h.parse::<u32>().is_ok()),
            "--fps" | "--kbps" => v.parse::<u32>().is_ok(),
            "--codec" => v == "h264" || v == "hevc",
            _ => false,
        };
        if !ok {
            return None;
        }
        out.push(k.to_string());
        out.push(v.to_string());
    }
    Some(out)
}

fn serve(path: &str, uid: Option<u32>) -> std::io::Result<()> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
        if let Some(u) = uid {
            let c = CString::new(dir.to_string_lossy().as_bytes()).unwrap();
            // SAFETY: valid NUL-terminated path.
            unsafe { chown(c.as_ptr(), u, u32::MAX) };
        }
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        if let Some(u) = uid {
            let c = CString::new(path).unwrap();
            // SAFETY: valid NUL-terminated path.
            unsafe { chown(c.as_ptr(), u, u32::MAX) };
        }
    }
    eprintln!("luch-kms: serving {path}");
    let exe = std::env::current_exe()?;
    for conn in listener.incoming() {
        let Ok(mut conn) = conn else { continue };
        // The first line, read byte by byte so nothing of the stream is swallowed.
        let mut line = Vec::new();
        let mut b = [0u8; 1];
        while line.len() < 256
            && conn.read(&mut b).map(|n| n == 1).unwrap_or(false)
            && b[0] != b'\n'
        {
            line.push(b[0]);
        }
        let Some(args) = options(&String::from_utf8_lossy(&line)) else {
            eprintln!("luch-kms: refused options");
            continue;
        };
        let Ok(input) = conn.try_clone() else {
            continue;
        };
        let _ = Command::new(&exe)
            .args(args)
            .stdin(Stdio::from(OwnedFd::from(input)))
            .stdout(Stdio::from(OwnedFd::from(conn)))
            .status();
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--serve") {
        let path = args.get(i + 1).cloned().unwrap_or_default();
        let uid = args
            .iter()
            .position(|a| a == "--uid")
            .and_then(|j| args.get(j + 1))
            .and_then(|v| v.parse().ok());
        if path.is_empty() || serve(&path, uid).is_err() {
            eprintln!("luch-kms: cannot serve {path}");
            std::process::exit(1);
        }
        return;
    }
    let args: Vec<CString> = args.into_iter().map(|a| CString::new(a).unwrap()).collect();
    let ptrs: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    // SAFETY: argv-style array of valid C strings that outlives the call.
    std::process::exit(unsafe { luch_kms_main(ptrs.len() as c_int, ptrs.as_ptr()) });
}
