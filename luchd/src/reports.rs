//! Crash and error reports a receiver keeps and hands to a sender that asks (`"want": ["reports"]`; luch-android
//! PROTOCOL.md, section 6). luchd keeps them in ~/.config/luch/reports, answers `reports_taken`, and shows a
//! notification that opens a ready-to-file issue. Nothing is sent anywhere without the person.

use std::{collections::HashMap, io, path::PathBuf};

use serde_json::{json, Value};
use zbus::{zvariant, Connection};

/// Where receiver reports are filed: the receiver's repository.
pub const ISSUES: &str = "https://altlinux.space/Luch/luch-android/issues/new";
/// A browser takes long URLs, but not endlessly: the stack is cut to keep the link under this.
const MAX_URL: usize = 7000;

fn dir() -> io::Result<PathBuf> {
    let d = crate::config_dir()?.join("reports");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// A report id is the receiver's UUID; anything else (a path, "..") is refused before it names a file.
pub fn valid_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Keeps the reports of a `{"t":"reports","items":[...]}` message; returns the ids to answer `reports_taken` with
/// (every valid one, also those kept before) and how many were new.
pub fn store(receiver: &str, items: &[Value]) -> (Vec<String>, usize) {
    let (mut ids, mut new) = (Vec::new(), 0);
    let Ok(d) = dir() else { return (ids, new) };
    for item in items {
        let Some(id) = item["id"].as_str().filter(|id| valid_id(id)) else {
            continue;
        };
        let path = d.join(format!("{id}.json"));
        if !path.exists() {
            let mut r = item.clone();
            r["receiver_model"] = json!(receiver);
            if std::fs::write(&path, r.to_string()).is_ok() {
                new += 1;
            } else {
                continue; // not kept: let the receiver offer it again
            }
        }
        ids.push(id.to_string());
    }
    (ids, new)
}

/// Kept reports, oldest first.
pub fn list() -> Vec<Value> {
    let mut out: Vec<Value> = dir()
        .and_then(std::fs::read_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect();
    out.sort_by_key(|r: &Value| r["time"].as_i64().unwrap_or(0));
    out
}

pub fn remove(id: &str) -> bool {
    valid_id(id)
        && dir()
            .map(|d| std::fs::remove_file(d.join(format!("{id}.json"))).is_ok())
            .unwrap_or(false)
}

/// One line for a list and a notification.
pub fn summary(r: &Value) -> String {
    let kind = if r["kind"] == "crash" {
        "crashed"
    } else {
        "error"
    };
    let count = r["count"].as_i64().unwrap_or(1);
    format!(
        "{} {kind}: {}{} (Luch {}, Android {})",
        r["receiver_model"]
            .as_str()
            .or(r["device"].as_str())
            .unwrap_or("receiver"),
        r["what"].as_str().unwrap_or("?"),
        if count > 1 {
            format!(", {count} times")
        } else {
            String::new()
        },
        r["app"].as_str().unwrap_or("?"),
        r["android"].as_str().unwrap_or("?"),
    )
}

/// The issue to file, with the report in its body. The report holds no addresses or names (the receiver scrubs
/// them), and the person sees everything before sending.
pub fn issue_url(r: &Value) -> String {
    let title = format!(
        "{}: {}",
        if r["kind"] == "crash" {
            "Crash"
        } else {
            "Error"
        },
        r["what"].as_str().unwrap_or("?")
    );
    let head = format!(
        "Reported by the receiver, filed from luchd.\n\n\
         - Luch: {}\n- Android: {} (SDK {})\n- Device: {} ({})\n- Times: {}\n- Thread: {}\n- Session: `{}`\n\n",
        r["app"].as_str().unwrap_or("?"),
        r["android"].as_str().unwrap_or("?"),
        r["sdk"],
        r["device"].as_str().unwrap_or("?"),
        r["hardware"].as_str().unwrap_or("?"),
        r["count"],
        r["thread"].as_str().unwrap_or("?"),
        r["details"],
    );
    let mut stack = r["stack"].as_str().unwrap_or("").to_string();
    loop {
        let body = format!("{head}```\n{stack}\n```\n");
        let url = format!("{ISSUES}?title={}&body={}", encode(&title), encode(&body));
        if url.len() <= MAX_URL || stack.is_empty() {
            return url;
        }
        // Keep the top of the stack: the exception and the frames nearest to it.
        let keep = stack.len() * 3 / 4;
        let cut = stack
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|i| *i <= keep)
            .last()
            .unwrap_or(0);
        stack.truncate(cut);
        stack.push_str("\n…");
    }
}

/// Percent-encoding for a URL query value.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A desktop notification; clicking it (or its button) opens `url` in the browser. Best effort.
pub async fn notify(conn: &Connection, summary: &str, body: &str, url: Option<String>) {
    let Ok(proxy) = zbus::Proxy::new(
        conn,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )
    .await
    else {
        return;
    };
    let actions: Vec<&str> = if url.is_some() {
        vec!["default", "Report", "report", "Report"]
    } else {
        vec![]
    };
    let hints: HashMap<&str, zvariant::Value> = HashMap::new();
    let id: u32 = match proxy
        .call(
            "Notify",
            &(
                "Luch",
                0u32,
                "video-display",
                summary,
                body,
                actions,
                hints,
                -1i32,
            ),
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            eprintln!("[luchd] notification: {e}");
            return;
        }
    };
    let Some(url) = url else { return };
    tokio::spawn(async move {
        use futures_util::StreamExt;
        let Ok(mut clicks) = proxy.receive_signal("ActionInvoked").await else {
            return;
        };
        let wait = tokio::time::timeout(std::time::Duration::from_secs(3600), async {
            while let Some(m) = clicks.next().await {
                if let Ok((nid, _action)) = m.body().deserialize::<(u32, String)>() {
                    if nid == id {
                        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
                        return;
                    }
                }
            }
        });
        let _ = wait.await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Value {
        json!({"id": "3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47", "kind": "error", "time": 1, "count": 2,
               "what": "no decoder for video/hevc", "thread": "luch-video", "stack": "java.io.IOException: x\n\tat a.b(C.java:1)",
               "details": {"codec": "video/hevc"}, "app": "0.4", "android": "5.1", "sdk": 22,
               "device": "Acme Box", "hardware": "mt8173", "receiver_model": "Acme Box"})
    }

    #[test]
    fn only_uuids_name_files() {
        assert!(valid_id("3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47"));
        assert!(!valid_id("../../.bashrc"));
        assert!(!valid_id("3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47/../x"));
        assert!(!valid_id(""));
    }

    #[test]
    fn the_issue_link_carries_the_report() {
        let url = issue_url(&report());
        assert!(url.starts_with(ISSUES));
        assert!(url.contains("title=Error%3A%20no%20decoder%20for%20video%2Fhevc"));
        assert!(url.contains("Android%3A%205.1%20%28SDK%2022%29"));
        assert!(url.contains("IOException"));
    }

    #[test]
    fn a_long_stack_is_cut_to_fit_a_link() {
        let mut r = report();
        r["stack"] = json!("\tat x.Y.z(Y.java:1)\n".repeat(2000));
        let url = issue_url(&r);
        assert!(url.len() <= MAX_URL, "{}", url.len());
        assert!(url.contains("%E2%80%A6")); // the "…" marking the cut
    }

    #[test]
    fn summary_reads_like_a_sentence() {
        assert_eq!(
            summary(&report()),
            "Acme Box error: no decoder for video/hevc, 2 times (Luch 0.4, Android 5.1)"
        );
    }

    #[test]
    fn stored_reports_are_listed_and_removed() {
        let _env = crate::tests::CONFIG_HOME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("luch-reports-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", &home);
        let mut bad = report();
        bad["id"] = json!("../evil");
        let (ids, new) = store("Acme Box", &[report(), bad]);
        assert_eq!(ids, vec!["3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47"]);
        assert_eq!(new, 1);
        assert_eq!(
            store("Acme Box", &[report()]).1,
            0,
            "a report offered again is not kept twice"
        );
        assert_eq!(list().len(), 1);
        assert!(remove("3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47"));
        assert!(list().is_empty());
        let _ = std::fs::remove_dir_all(home);
    }
}
