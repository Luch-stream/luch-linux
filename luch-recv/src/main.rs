//! luch-recv: a Luch receiver for Linux. It announces itself like a TV does, takes a sender's screen (a phone, another
//! computer) and shows it in a window of its own; a compositor can float that window (app id `luch-recv`).

mod control;
mod player;

use std::sync::Arc;
use tokio::net::TcpListener;
use zbus::{zvariant::OwnedObjectPath, Connection};

const AVAHI: &str = "org.freedesktop.Avahi";
const AVAHI_SERVER: &str = "org.freedesktop.Avahi.Server";

fn identity() -> control::Identity {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .map(|d| d.join("luch"));
    let file = dir.as_ref().map(|d| d.join("receiver-id"));
    let id = file
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let id = uuid::Uuid::new_v4().to_string();
            if let (Some(d), Some(f)) = (&dir, &file) {
                let _ = std::fs::create_dir_all(d);
                let _ = std::fs::write(f, &id);
            }
            id
        });
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "linux".into());
    control::Identity { id, name }
}

async fn announce(
    ident: &control::Identity,
    port: u16,
) -> zbus::Result<(Connection, OwnedObjectPath)> {
    let sys = Connection::system().await?;
    let group: OwnedObjectPath = sys
        .call_method(Some(AVAHI), "/", Some(AVAHI_SERVER), "EntryGroupNew", &())
        .await?
        .body()
        .deserialize()?;
    let txt: Vec<Vec<u8>> = [
        format!("v={}", luch_proto::VERSION),
        format!("id={}", ident.id),
        format!("name={}", ident.name),
        "model=Linux".to_string(),
        "dec=avc,hevc".to_string(),
    ]
    .into_iter()
    .map(String::into_bytes)
    .collect();
    let eg = Some("org.freedesktop.Avahi.EntryGroup");
    let args = (
        -1i32,
        0i32,
        0u32,
        format!("{} \u{b7} Luch", ident.name),
        luch_proto::RECEIVER_TYPE,
        "",
        "",
        port,
        txt,
    );
    sys.call_method(Some(AVAHI), &group, eg, "AddService", &args)
        .await?;
    sys.call_method(Some(AVAHI), &group, eg, "Commit", &())
        .await?;
    Ok((sys, group))
}

#[tokio::main]
async fn main() {
    gstreamer::glib::set_prgname(Some("luch-recv"));
    let ident = Arc::new(identity());
    let listener = match TcpListener::bind(("0.0.0.0", luch_proto::CONTROL_PORT)).await {
        Ok(l) => l,
        Err(_) => TcpListener::bind(("0.0.0.0", 0))
            .await
            .expect("no free port"),
    };
    let port = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or(luch_proto::CONTROL_PORT);
    eprintln!("[luch-recv] {} id={} on port {port}", ident.name, ident.id);
    // Kept alive: the announcement goes away with the connection.
    let _announced = match announce(&ident, port).await {
        Ok(a) => Some(a),
        Err(e) => {
            eprintln!(
                "[luch-recv] cannot announce on the network ({e}); senders must use the address"
            );
            None
        }
    };
    control::serve(listener, ident).await;
}
