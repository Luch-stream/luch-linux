//! The latency lab's sender side (docs/latency-lab.md): a page for this computer's screen that counts its vsyncs in
//! Gray code beside a QR code of how the stream is set up, served on the loopback interface only.
//!
//! A browser page, not a window of our own: every desktop has a browser that paints on vsync and says when
//! (`requestAnimationFrame`), goes full screen with a key on any compositor (GNOME, KDE, Niri, X11), and luchd stays
//! free of a toolkit. The page only draws; luchd hands it the layout and the QR code (from luch-latency, the same code
//! the analyser reads with), so the two can never disagree.

use std::collections::HashMap;
use std::io;

use luch_latency::pattern::{self, Meta, Qr, Role};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::Shared;

const PAGE: &str = include_str!("latency.html");

/// A running lab: its page's address and the server behind it, stopped when this is dropped.
pub struct Lab {
    pub id: String,
    pub url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Lab {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Serves the page for receiver `id` on a free port of 127.0.0.1.
pub async fn open(st: Shared, id: String) -> io::Result<Lab> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let url = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
    let who = id.clone();
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (st, id) = (st.clone(), who.clone());
            tokio::spawn(async move {
                let _ = answer(stream, st, id).await;
            });
        }
    });
    Ok(Lab { id, url, task })
}

async fn answer(mut stream: TcpStream, st: Shared, id: String) -> io::Result<()> {
    let mut head = Vec::with_capacity(512);
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= 8192 {
            return Ok(());
        }
        head.push(stream.read_u8().await?);
    }
    let head = String::from_utf8_lossy(&head);
    let target = head.split_whitespace().nth(1).unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (status, kind, body) = match path {
        "/" => ("200 OK", "text/html; charset=utf-8", PAGE.to_string()),
        "/meta" => {
            let q = params(query);
            let (stats, mode) = {
                let s = st.lock().await;
                let stats = s
                    .stats
                    .get(&id)
                    .map(|v| v.lock().unwrap().clone())
                    .unwrap_or(Value::Null);
                (stats, s.sources.get(&id).cloned().unwrap_or_default())
            };
            let hz = q.get("hz").and_then(|v| v.parse().ok());
            let missed = q.get("miss").and_then(|v| v.parse().ok());
            (
                "200 OK",
                "application/json",
                page_data(&sender_meta(&stats, &mode, hz, missed)).to_string(),
            )
        }
        _ => ("404 Not Found", "text/plain", "not here\n".to_string()),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

fn params(query: &str) -> HashMap<&str, &str> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .collect()
}

/// What the sender's QR code says. Short keys keep the code small, and so its modules big for the camera; the
/// analyser's report spells them out.
pub fn sender_meta(stats: &Value, mode: &str, hz: Option<f64>, missed: Option<u64>) -> Meta {
    let s = &stats["sender"];
    let num = |v: &Value| v.as_f64().map(|x| format!("{x:.1}")).unwrap_or_default();
    let codec = match s["encoder"].as_str() {
        Some(e) if e.starts_with("hevc") => "hevc",
        Some(e) if e.starts_with("h264") => "h264",
        _ => "",
    };
    Meta::new(Role::Sender)
        .with("v", luch_proto::APP)
        .with("m", mode)
        .with("c", codec)
        .with("b", num(&s["mbps"]))
        .with("r", s["res"].as_str().unwrap_or(""))
        .with("p", num(&s["rtt_ms"]))
        .with("h", num(&s["host_ms"]))
        .with("e", num(&stats["est_ms"]))
        .with("hz", hz.map(|h| format!("{h:.2}")).unwrap_or_default())
        .with("x", missed.map(|m| m.to_string()).unwrap_or_default())
}

/// Everything the page draws from: the layout, the QR code's modules and its text.
pub fn page_data(meta: &Meta) -> Value {
    let text = meta.to_text();
    json!({"layout": pattern::layout_json(), "qr": Qr::encode(&text).rows(), "text": text})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_qr_code_says_how_the_stream_is_set_up() {
        let stats = json!({"sender": {"rtt_ms": 3.7, "host_ms": 5.62, "mbps": 21.4, "encoder": "hevc_vaapi",
                                      "res": "1920x1080"}, "est_ms": 13.5});
        let m = sender_meta(&stats, "fast", Some(59.94), Some(2));
        assert_eq!(
            m.to_text(),
            format!(
                "LUCH1;S;v={};m=fast;c=hevc;b=21.4;r=1920x1080;p=3.7;h=5.6;e=13.5;hz=59.94;x=2",
                luch_proto::APP
            )
        );
        // Not mirroring yet: what is known, nothing made up.
        let idle = sender_meta(&Value::Null, "", None, None);
        assert_eq!(idle.to_text(), format!("LUCH1;S;v={}", luch_proto::APP));
        let page = page_data(&m);
        assert_eq!(
            page["qr"].as_array().unwrap().len(),
            Qr::encode(&m.to_text()).size
        );
        assert_eq!(page["layout"]["bits"], 12);
    }

    #[tokio::test]
    async fn serves_the_page_and_its_data_on_loopback_only() {
        let st = Shared::default();
        let lab = open(st, "tv".into()).await.unwrap();
        assert!(lab.url.starts_with("http://127.0.0.1:"));
        let get = |path: &'static str| {
            let url = lab.url.clone();
            async move {
                let addr = url
                    .trim_start_matches("http://")
                    .trim_end_matches('/')
                    .to_string();
                let mut s = TcpStream::connect(addr).await.unwrap();
                s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
                let mut out = String::new();
                s.read_to_string(&mut out).await.unwrap();
                out
            }
        };
        let page = get("/").await;
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("requestAnimationFrame"));
        let meta = get("/meta?hz=60.00&miss=0").await;
        let body: Value = serde_json::from_str(meta.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert!(
            body["text"].as_str().unwrap().ends_with(";hz=60.00;x=0"),
            "{body}"
        );
        assert!(get("/etc/passwd").await.starts_with("HTTP/1.1 404"));
    }
}
