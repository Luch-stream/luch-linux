//! `luchctl native-host`: the browser extension's way to luchd (Chrome and Firefox native messaging). The browser
//! starts it and speaks over stdin/stdout, each message a 4-byte native-endian length and then JSON.
//!
//! In:  {"id":1,"op":"receivers"}                         → {"id":1,"ok":[{"id","name","model","connected","kind"}]}
//!      {"id":2,"op":"cast","receiver":R,"items":[url],"options":{...Cast options}} → {"id":2,"ok":"media"}
//!      {"id":3,"op":"cmd","receiver":R,"cmd":{"do":"pause"}} → {"id":3,"ok":true}
//!      {"id":4,"op":"state","receiver":R}                → {"id":4,"ok":{media_state or {}}}
//!      {"id":5,"op":"stop","receiver":R}                 → {"id":5,"ok":true}
//!      {"id":6,"op":"version"}                           → {"id":6,"ok":"0.4.0"}
//! Out, unasked: {"event":"state","receiver":R,"state":{...}} for every MediaStateChanged.
//! A failure answers {"id":n,"error":"<luchd's reason>"}.

use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::Luch1Proxy;

/// Browsers cap a message to the host at 4 GB and from it at 1 MB; ours are small.
const MAX_IN: u32 = 1024 * 1024;

pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Value>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_ne_bytes(len);
    if n > MAX_IN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too long",
        ));
    }
    let mut buf = vec![0u8; n as usize];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_frame(w: &mut impl Write, v: &Value) -> io::Result<()> {
    let s = v.to_string();
    w.write_all(&(s.len() as u32).to_ne_bytes())?;
    w.write_all(s.as_bytes())?;
    w.flush()
}

fn err_text(e: zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(_, Some(detail), _) => detail,
        other => other.to_string(),
    }
}

/// One request to luchd; the answer's `ok` value or the reason it failed.
pub async fn handle(p: &Luch1Proxy<'_>, req: &Value) -> Result<Value, String> {
    let receiver = || {
        req["receiver"]
            .as_str()
            .map(String::from)
            .ok_or("no receiver".to_string())
    };
    match req["op"].as_str().unwrap_or("") {
        "version" => Ok(json!(env!("CARGO_PKG_VERSION"))),
        "receivers" => {
            let _ = p.refresh().await;
            let rows = p.receivers().await.map_err(err_text)?;
            let mut list = Vec::new();
            for (id, name, model, _, _, _, connected, mirroring) in rows {
                // Whether it plays media, from its last welcome; null until luchd has talked to it once.
                let w: Value = serde_json::from_str(&p.capabilities(&id).await.unwrap_or_default())
                    .unwrap_or(json!({}));
                let media = w["modes"]
                    .as_array()
                    .map(|m| json!(m.iter().any(|x| x == "media")))
                    .unwrap_or(Value::Null);
                let kind = crate::kind_of(&id);
                list.push(
                    json!({"id": id, "name": name, "model": model, "connected": connected,
                                 "mirroring": mirroring, "media": media, "kind": kind}),
                );
            }
            Ok(Value::Array(list))
        }
        "cast" => {
            let items: Vec<String> = req["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(String::from))
                // The page may only cast links, never this computer's files.
                .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
                .collect();
            if items.is_empty() {
                return Err("nothing to cast".into());
            }
            let options = if req["options"].is_object() {
                req["options"].to_string()
            } else {
                String::new()
            };
            let kind = p
                .cast(&receiver()?, &items, &options)
                .await
                .map_err(err_text)?;
            Ok(json!(kind))
        }
        "cmd" => {
            if !req["cmd"].is_object() {
                return Err("no cmd".into());
            }
            p.media_cmd(&receiver()?, &req["cmd"].to_string())
                .await
                .map_err(err_text)?;
            Ok(json!(true))
        }
        "state" => {
            let s = p.media_state(&receiver()?).await.map_err(err_text)?;
            Ok(serde_json::from_str(&s).unwrap_or(json!({})))
        }
        "stop" => {
            p.stop_cast(&receiver()?).await.map_err(err_text)?;
            Ok(json!(true))
        }
        op => Err(format!("unknown op {op}")),
    }
}

pub async fn run(p: Luch1Proxy<'static>) -> Result<(), String> {
    let out = Arc::new(Mutex::new(io::stdout()));
    // Media state from luchd, passed on as it comes.
    {
        let out = out.clone();
        let mut changes = p
            .receive_media_state_changed()
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(async move {
            while let Some(c) = changes.next().await {
                let Ok(a) = c.args() else { continue };
                let state: Value = serde_json::from_str(a.state).unwrap_or(json!({}));
                let msg = json!({"event": "state", "receiver": a.id, "state": state});
                if write_frame(&mut *out.lock().unwrap(), &msg).is_err() {
                    std::process::exit(0); // the browser has gone
                }
            }
        });
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(16);
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        while let Ok(Some(v)) = read_frame(&mut stdin) {
            if tx.blocking_send(v).is_err() {
                break;
            }
        }
    });
    while let Some(req) = rx.recv().await {
        let p = p.clone();
        let out = out.clone();
        tokio::spawn(async move {
            let id = req["id"].clone();
            let msg = match handle(&p, &req).await {
                Ok(v) => json!({"id": id, "ok": v}),
                Err(e) => json!({"id": id, "error": e}),
            };
            let _ = write_frame(&mut *out.lock().unwrap(), &msg);
        });
    }
    Ok(()) // stdin closed: the extension let go of us
}

/// The browsers' host manifest, for `luchctl native-host install`.
pub fn manifest(path: &str, firefox: bool) -> Value {
    let mut m = json!({
        "name": "dev.luch.host",
        "description": "Luch: cast a video from a web page to a TV",
        "path": path,
        "type": "stdio",
    });
    if firefox {
        m["allowed_extensions"] = json!(["luch@toxblh.github.io"]);
    } else {
        m["allowed_origins"] = json!([format!("chrome-extension://{}/", CHROME_ID)]);
    }
    m
}

/// The id Chromium gives the extension, fixed by the `key` in its manifest.json.
pub const CHROME_ID: &str = "enoggfebgncdlpimolmheljkneecempd";

/// Where each browser looks for host manifests of the current user.
pub fn manifest_dirs(home: &str) -> Vec<(String, bool)> {
    [
        (".config/google-chrome/NativeMessagingHosts", false),
        (".config/chromium/NativeMessagingHosts", false),
        (
            ".config/BraveSoftware/Brave-Browser/NativeMessagingHosts",
            false,
        ),
        (".config/microsoft-edge/NativeMessagingHosts", false),
        (".config/yandex-browser/NativeMessagingHosts", false),
        (".mozilla/native-messaging-hosts", true),
        (
            ".var/app/org.mozilla.firefox/.mozilla/native-messaging-hosts",
            true,
        ),
        (
            ".var/app/org.chromium.Chromium/config/chromium/NativeMessagingHosts",
            false,
        ),
    ]
    .iter()
    .map(|(d, f)| (format!("{home}/{d}"), *f))
    .collect()
}

/// Browsers installed as a Flatpak: (app id, where it looks for host manifests inside its data, Firefox).
pub const FLATPAKS: [(&str, &str, bool); 5] = [
    (
        "com.google.Chrome",
        "config/google-chrome/NativeMessagingHosts",
        false,
    ),
    (
        "org.chromium.Chromium",
        "config/chromium/NativeMessagingHosts",
        false,
    ),
    (
        "com.brave.Browser",
        "config/BraveSoftware/Brave-Browser/NativeMessagingHosts",
        false,
    ),
    (
        "com.microsoft.Edge",
        "config/microsoft-edge/NativeMessagingHosts",
        false,
    ),
    (
        "org.mozilla.firefox",
        ".mozilla/native-messaging-hosts",
        true,
    ),
];

/// The script a sandboxed browser starts: it runs luchctl outside the sandbox, which needs the browser to be allowed
/// to talk to org.freedesktop.Flatpak (`flatpak override --user --talk-name=org.freedesktop.Flatpak <app>`).
pub fn flatpak_wrapper(exe: &str) -> String {
    format!("#!/bin/sh\n# Luch: the browser extension's host, run outside the Flatpak sandbox.\nexec flatpak-spawn --host '{}' \"$@\"\n", exe.replace('\'', ""))
}

/// Writes the host manifest for every browser whose profile directory exists; returns where. For a Flatpak browser
/// also a wrapper script inside its data, and the line that lets it run it (`needs`).
pub fn install(exe: &str, home: &str) -> io::Result<Vec<String>> {
    let mut done = Vec::new();
    for (app, dir, firefox) in FLATPAKS {
        let data = format!("{home}/.var/app/{app}");
        if !std::path::Path::new(&data).exists() {
            continue;
        }
        let wrapper = format!("{data}/luch-host.sh");
        std::fs::write(&wrapper, flatpak_wrapper(exe))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;
        }
        let dir = format!("{data}/{dir}");
        std::fs::create_dir_all(&dir)?;
        let file = format!("{dir}/dev.luch.host.json");
        std::fs::write(
            &file,
            serde_json::to_string_pretty(&manifest(&wrapper, firefox)).unwrap(),
        )?;
        done.push(format!("{file} (Flatpak; also run: flatpak override --user --talk-name=org.freedesktop.Flatpak {app})"));
    }
    for (dir, firefox) in manifest_dirs(home) {
        let parent = std::path::Path::new(&dir).parent().map(|p| p.to_path_buf());
        if !parent.is_some_and(|p| p.exists()) {
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        let file = format!("{dir}/dev.luch.host.json");
        std::fs::write(
            &file,
            serde_json::to_string_pretty(&manifest(exe, firefox)).unwrap(),
        )?;
        done.push(file);
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_end() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &json!({"id": 1, "op": "receivers"})).unwrap();
        write_frame(&mut buf, &json!({"id": 2, "op": "état"})).unwrap();
        let mut r = io::Cursor::new(buf);
        assert_eq!(read_frame(&mut r).unwrap().unwrap()["op"], "receivers");
        assert_eq!(read_frame(&mut r).unwrap().unwrap()["op"], "état");
        assert!(read_frame(&mut r).unwrap().is_none());
    }

    #[test]
    fn oversized_and_broken_frames_are_refused() {
        let mut r = io::Cursor::new((MAX_IN + 1).to_ne_bytes().to_vec());
        assert!(read_frame(&mut r).is_err());
        let mut bad = 3u32.to_ne_bytes().to_vec();
        bad.extend_from_slice(b"{x}");
        assert!(read_frame(&mut io::Cursor::new(bad)).is_err());
    }

    #[test]
    fn manifests_and_where_they_go() {
        let home = std::env::temp_dir().join(format!("luch-nm-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".mozilla")).unwrap();
        std::fs::create_dir_all(home.join(".config/chromium")).unwrap();
        let h = home.to_string_lossy();
        std::fs::create_dir_all(home.join(".var/app/com.google.Chrome")).unwrap();
        let done = install("/usr/bin/luchctl-host", &h).unwrap();
        assert_eq!(done.len(), 3, "{done:?}");
        let wrapper = home.join(".var/app/com.google.Chrome/luch-host.sh");
        let script = std::fs::read_to_string(&wrapper).unwrap();
        assert!(
            script.contains("exec flatpak-spawn --host '/usr/bin/luchctl-host' \"$@\""),
            "{script}"
        );
        let fp: Value = serde_json::from_str(
            &std::fs::read_to_string(
                home.join(".var/app/com.google.Chrome/config/google-chrome/NativeMessagingHosts/dev.luch.host.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(fp["path"], wrapper.to_string_lossy().as_ref());
        let ff: Value = serde_json::from_str(
            &std::fs::read_to_string(
                home.join(".mozilla/native-messaging-hosts/dev.luch.host.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(ff["allowed_extensions"][0], "luch@toxblh.github.io");
        assert_eq!(ff["path"], "/usr/bin/luchctl-host");
        let cr: Value = serde_json::from_str(
            &std::fs::read_to_string(
                home.join(".config/chromium/NativeMessagingHosts/dev.luch.host.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            cr["allowed_origins"][0],
            format!("chrome-extension://{CHROME_ID}/")
        );
        let _ = std::fs::remove_dir_all(home);
    }
}
