//! luchctl — CLI for luchd over D-Bus (dev.luch.Luch1).

use futures_util::StreamExt;
use luch_proto::ReceiverRow;

mod native;

#[zbus::proxy(
    interface = "dev.luch.Luch1",
    default_service = "dev.luch.Luch1",
    default_path = "/dev/luch/Luch1"
)]
trait Luch1 {
    fn receivers(&self) -> zbus::Result<Vec<ReceiverRow>>;
    fn refresh(&self) -> zbus::Result<()>;
    fn connect(&self, id: &str) -> zbus::Result<String>;
    fn disconnect(&self, id: &str) -> zbus::Result<()>;
    fn mirror(&self, id: &str, source: &str) -> zbus::Result<()>;
    fn stop_mirror(&self, id: &str) -> zbus::Result<()>;
    fn stats(&self, id: &str) -> zbus::Result<String>;
    fn capabilities(&self, id: &str) -> zbus::Result<String>;
    fn info(&self, id: &str) -> zbus::Result<String>;
    fn set_audio(&self, id: &str, mode: &str) -> zbus::Result<()>;
    fn reports(&self) -> zbus::Result<String>;
    fn report_url(&self, id: &str) -> zbus::Result<String>;
    fn dismiss_report(&self, id: &str) -> zbus::Result<()>;
    fn known(&self) -> zbus::Result<String>;
    fn wake(&self, id: &str) -> zbus::Result<String>;
    fn learn_wake(&self, id: &str, seconds: u32) -> zbus::Result<String>;
    fn set_wake(&self, id: &str, kind: &str, value: &str) -> zbus::Result<()>;
    fn cast(&self, id: &str, items: &[String], options: &str) -> zbus::Result<String>;
    fn media_cmd(&self, id: &str, cmd: &str) -> zbus::Result<()>;
    fn media_state(&self, id: &str) -> zbus::Result<String>;
    fn stop_cast(&self, id: &str) -> zbus::Result<()>;
    fn sleep(&self, id: &str) -> zbus::Result<()>;
    fn set_volume(&self, id: &str, what: &str) -> zbus::Result<String>;
    fn get_volume(&self, id: &str) -> zbus::Result<String>;
    fn doc_page(&self, id: &str, what: &str) -> zbus::Result<String>;
    fn present(&self, id: &str, file: &str) -> zbus::Result<String>;
    fn annotate(&self, id: &str, msg: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn quiet(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn set_quiet(&self, mode: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    fn receivers_changed(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn media_state_changed(&self, id: &str, state: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn sender_id(&self) -> zbus::Result<String>;
}

const USAGE: &str = "usage: luchctl [list | refresh | connect [r] | disconnect [r] | mirror [r] [fast|monitor|window|virtual] | stop [r] | stats [r] | watch | id
                | info [r] | audio [r] auto|on|off|sync | reports | report <n> [open|dismiss]
                | known | wake [r] | wake <r> learn | wake <r> mac|ble <value>|off | sleep [r]
                | cast [r] <file|link>... [--title T] [--start m:ss] [--sub file] [--relay] [--paused]
                | media [r] [play|pause|toggle|seek m:ss|skip s|rate x|audio id|sub id|sub off|watch]
                | volume [r] [0..100|+|-|mute|unmute] | page [r] next|prev|<n> | blank [r] on|off
                | present [r] <file> | quiet [ask|on|off] | browser install]
       r = receiver id or name, optional when there is only one; mirror defaults to fast; no arguments: interactive
       wake: turns on a receiver luchd has talked to before (Wake-on-LAN, or the remote's Bluetooth signal that
       `wake <r> learn` picks up while you press the remote's power key with the receiver off)
       cast: photos, or a video or sound the receiver plays itself in its own quality; a link is fetched by the
       receiver, or with --relay through this computer (a VPN, a login); media: control it, no word: its state
       mirror audio: this computer's sound only, the TV as a speaker showing what plays (its remote's keys control
       the player here); cast also takes PDF, office files, text and code (others through LibreOffice)
       present: shows a PDF or a presentation on the receiver and opens the presenter view here (current and
       next slide, notes, timer; the mouse is a laser pointer, dragging draws, a clicker turns the slides)
       quiet: hide notification banners while presenting or mirroring (ask: the GNOME extension asks once)
       browser install: lets the Luch browser extension reach luchd (Chrome, Chromium, Brave, Edge, Yandex, Firefox)";

const AUDIO: [&str; 4] = ["auto", "on", "off", "sync"];

const SOURCES: [&str; 5] = ["fast", "monitor", "window", "virtual", "audio"];

fn print(rows: &[ReceiverRow]) {
    if rows.is_empty() {
        println!("(no receivers)");
        return;
    }
    println!(
        "{:<38} {:<20} {:<16} {:<21} {:<10} STATE",
        "ID", "NAME", "MODEL", "ADDRESS", "IFACE"
    );
    for (id, name, model, addr, ifname, port, connected, mirroring) in rows {
        let state = if *mirroring {
            "mirroring"
        } else if *connected {
            "connected"
        } else {
            "-"
        };
        println!(
            "{id:<38} {name:<20} {model:<16} {:<21} {ifname:<10} {state}",
            format!("{addr}:{port}")
        );
    }
}

/// Every receiver luchd has talked to (Known), and one of them by id or name; with no key, the only one.
async fn known(
    p: &Luch1Proxy<'_>,
    key: Option<&str>,
) -> Result<(Vec<serde_json::Value>, Option<serde_json::Value>), String> {
    let all: Vec<serde_json::Value> =
        serde_json::from_str(&p.known().await.map_err(|e| e.to_string())?).unwrap_or_default();
    let hits: Vec<_> = all
        .iter()
        .filter(|r| match key {
            None => true,
            Some(k) => {
                r["id"] == k
                    || r["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(k))
            }
        })
        .cloned()
        .collect();
    let one = match (key, hits.as_slice()) {
        (_, [one]) => Some(one.clone()),
        (None, _) => None,
        (Some(k), []) => {
            return Err(format!(
                "luchd has not talked to '{k}' yet (see `luchctl known`)"
            ))
        }
        (Some(k), _) => return Err(format!("'{k}' is ambiguous, use the id")),
    };
    Ok((all, one))
}

fn need_one(
    one: Option<serde_json::Value>,
    all: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    one.ok_or_else(|| {
        if all.is_empty() {
            "no known receivers: connect to one once while it is on".into()
        } else {
            format!(
                "{} known receivers, name one (see `luchctl known`)",
                all.len()
            )
        }
    })
}

/// Receiver by id or name; with no key, the only receiver on the LAN.
async fn find_id(p: &Luch1Proxy<'_>, key: Option<&str>) -> Result<String, String> {
    let rows = p.receivers().await.map_err(|e| e.to_string())?;
    let Some(key) = key else {
        return match rows.as_slice() {
            [one] => Ok(one.0.clone()),
            [] => Err("no receivers found (is the receiver app running?)".into()),
            _ => Err(format!(
                "{} receivers on the LAN, name one (see `luchctl list`)",
                rows.len()
            )),
        };
    };
    let hits: Vec<_> = rows
        .iter()
        .filter(|r| r.0 == key || r.1.eq_ignore_ascii_case(key))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one.0.clone()),
        [] => Err(format!("no receiver '{key}' (see `luchctl list`)")),
        _ => Err(format!("'{key}' is ambiguous, use the id")),
    }
}

async fn run(args: &[String]) -> Result<(), String> {
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| e.to_string())?;
    let p = Luch1Proxy::new(&conn).await.map_err(|e| e.to_string())?;
    // luchd's own refusals as they are; "is luchd running?" only when nobody answered.
    let e = |e: zbus::Error| match &e {
        zbus::Error::MethodError(name, Some(detail), _)
            if !name.as_str().contains("ServiceUnknown")
                && !name.as_str().contains("NameHasNoOwner") =>
        {
            detail.clone()
        }
        _ => format!("{e} (is luchd running?)"),
    };
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["list"] => {
            p.refresh().await.map_err(e)?;
            print(&p.receivers().await.map_err(e)?);
        }
        ["refresh"] => p.refresh().await.map_err(e)?,
        ["id"] => println!("{}", p.sender_id().await.map_err(e)?),
        ["connect", key @ ..] if key.len() <= 1 => {
            let id = find_id(&p, key.first().copied()).await?;
            let welcome = p.connect(&id).await.map_err(e)?;
            let pretty = serde_json::from_str::<serde_json::Value>(&welcome)
                .and_then(|v| serde_json::to_string_pretty(&v))
                .unwrap_or(welcome);
            println!("{pretty}");
        }
        ["disconnect", key @ ..] if key.len() <= 1 => p
            .disconnect(&find_id(&p, key.first().copied()).await?)
            .await
            .map_err(e)?,
        ["mirror"] => p
            .mirror(&find_id(&p, None).await?, "fast")
            .await
            .map_err(e)?,
        // `mirror fast` with a single receiver: a known source word that isn't a receiver name.
        ["mirror", one] if SOURCES.contains(one) && find_id(&p, Some(one)).await.is_err() => {
            p.mirror(&find_id(&p, None).await?, one).await.map_err(e)?
        }
        ["mirror", key] => p
            .mirror(&find_id(&p, Some(key)).await?, "fast")
            .await
            .map_err(e)?,
        ["mirror", key, source] => p
            .mirror(&find_id(&p, Some(key)).await?, source)
            .await
            .map_err(e)?,
        ["stats", key @ ..] if key.len() <= 1 => {
            let id = find_id(&p, key.first().copied()).await?;
            loop {
                println!("{}", stats_line(&p.stats(&id).await.map_err(e)?));
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        ["stop", key @ ..] if key.len() <= 1 => {
            let id = find_id(&p, key.first().copied()).await?;
            // Mirroring, or else whatever was cast.
            if p.stop_mirror(&id).await.is_err() {
                p.stop_cast(&id).await.map_err(e)?;
            }
        }
        ["volume", rest @ ..] if rest.len() <= 2 => {
            let is_word = |w: &str| {
                matches!(w, "+" | "-" | "up" | "down" | "mute" | "unmute")
                    || w.trim_end_matches('%').parse::<u8>().is_ok()
            };
            let (key, what) = match rest {
                [] => (None, None),
                [w] if is_word(w) => (None, Some(*w)),
                [k] => (Some(*k), None),
                [k, w] => (Some(*k), Some(*w)),
                _ => unreachable!(),
            };
            let id = find_id(&p, key).await?;
            let v = match what {
                Some(w) => p.set_volume(&id, w).await.map_err(e)?,
                None => p.get_volume(&id).await.map_err(e)?,
            };
            let v: serde_json::Value = serde_json::from_str(&v).unwrap_or_default();
            match v["level"].as_u64() {
                Some(l) => println!("{l}%{}", if v["muted"] == true { " (muted)" } else { "" }),
                None => println!("unknown until the receiver tells (luchctl volume <r> +)"),
            }
        }
        ["page", rest @ ..] if !rest.is_empty() && rest.len() <= 2 => {
            let (key, what) = if rest.len() == 2 {
                (Some(rest[0]), rest[1])
            } else {
                (None, rest[0])
            };
            println!(
                "{}",
                p.doc_page(&find_id(&p, key).await?, what)
                    .await
                    .map_err(e)?
            );
        }
        ["present", rest @ ..] if !rest.is_empty() && rest.len() <= 2 => {
            let (key, file) = match rest {
                [k, f] => (Some(*k), *f),
                [f] => (None, *f),
                _ => unreachable!(),
            };
            let id = find_id(&p, key).await?;
            let url = p.present(&id, &absolute(file)).await.map_err(e)?;
            println!("presenter view: {url}");
            if std::process::Command::new("xdg-open")
                .arg(&url)
                .spawn()
                .is_err()
            {
                println!("(open it in a browser)");
            }
        }
        ["quiet"] => println!("{}", p.quiet().await.map_err(e)?),
        ["quiet", mode] if ["ask", "on", "off"].contains(mode) => {
            p.set_quiet(mode).await.map_err(e)?
        }
        ["blank", rest @ ..] if !rest.is_empty() && rest.len() <= 2 => {
            let (key, on) = match rest {
                [k, w] => (Some(*k), *w),
                [w] => (None, *w),
                _ => unreachable!(),
            };
            let on = match on {
                "on" => true,
                "off" => false,
                w => return Err(format!("blank on|off, not {w}")),
            };
            let msg = serde_json::json!({"t": "blank", "on": on}).to_string();
            p.annotate(&find_id(&p, key).await?, &msg)
                .await
                .map_err(e)?;
        }
        ["sleep", key @ ..] if key.len() <= 1 => {
            p.sleep(&find_id(&p, key.first().copied()).await?)
                .await
                .map_err(e)?;
        }
        ["cast", rest @ ..] if !rest.is_empty() => {
            let (key, rest) = match rest {
                [k, more @ ..] if !more.is_empty() && !looks_like_item(k) => (Some(*k), more),
                _ => (None, rest),
            };
            let (items, options) = cast_args(rest)?;
            let id = find_id(&p, key).await?;
            let kind = p.cast(&id, &items, &options.to_string()).await.map_err(e)?;
            println!(
                "{}",
                match kind.as_str() {
                    "media" => "playing on the receiver; `luchctl media` follows it",
                    "doc" => "the document is on the receiver; `luchctl page next` turns its pages",
                    "app" => "opened in the TV's own app",
                    _ => "on the receiver",
                }
            );
        }
        ["media", rest @ ..] => {
            let (key, words) = match rest {
                [k, more @ ..] if !MEDIA_WORDS.contains(k) => (Some(*k), more),
                _ => (None, rest),
            };
            let id = find_id(&p, key).await?;
            match words {
                [] => println!("{}", state_line(&p.media_state(&id).await.map_err(e)?)),
                ["watch"] => {
                    let mut changes = p.receive_media_state_changed().await.map_err(e)?;
                    println!("{}", state_line(&p.media_state(&id).await.map_err(e)?));
                    while let Some(c) = changes.next().await {
                        let Ok(a) = c.args() else { continue };
                        if a.id == id {
                            println!("{}", state_line(a.state));
                        }
                    }
                }
                w => p
                    .media_cmd(&id, &media_command(w)?.to_string())
                    .await
                    .map_err(e)?,
            }
        }
        ["info", key @ ..] if key.len() <= 1 => {
            let id = find_id(&p, key.first().copied()).await?;
            let v: serde_json::Value =
                serde_json::from_str(&p.info(&id).await.map_err(e)?).unwrap_or_default();
            let android = v["android"].as_str().unwrap_or("");
            println!(
                "{}",
                if android.is_empty() {
                    "Android version unknown until connected (luchctl connect)"
                } else {
                    android
                }
            );
            println!(
                "sound: {}{}",
                v["audio"].as_str().unwrap_or("auto"),
                v["audio_latency_ms"]
                    .as_u64()
                    .map(|ms| format!(", trails the picture by about {ms} ms"))
                    .unwrap_or_default()
            );
            for l in v["limits"].as_array().into_iter().flatten() {
                println!("  - {}", l.as_str().unwrap_or(""));
            }
        }
        ["audio", mode] if AUDIO.contains(mode) => p
            .set_audio(&find_id(&p, None).await?, mode)
            .await
            .map_err(e)?,
        ["audio", key, mode] if AUDIO.contains(mode) => p
            .set_audio(&find_id(&p, Some(key)).await?, mode)
            .await
            .map_err(e)?,
        ["reports"] => {
            let all: Vec<serde_json::Value> =
                serde_json::from_str(&p.reports().await.map_err(e)?).unwrap_or_default();
            if all.is_empty() {
                println!("no reports");
            }
            for (n, r) in all.iter().enumerate() {
                println!(
                    "{}. {} {}: {}{}",
                    n + 1,
                    r["receiver_model"]
                        .as_str()
                        .or(r["device"].as_str())
                        .unwrap_or("?"),
                    r["kind"].as_str().unwrap_or("?"),
                    r["what"].as_str().unwrap_or("?"),
                    match r["count"].as_i64() {
                        Some(c) if c > 1 => format!(" ({c} times)"),
                        _ => String::new(),
                    }
                );
            }
        }
        ["report", n, action @ ..] if action.len() <= 1 => {
            let all: Vec<serde_json::Value> =
                serde_json::from_str(&p.reports().await.map_err(e)?).unwrap_or_default();
            let r = n
                .parse::<usize>()
                .ok()
                .and_then(|n| all.get(n.wrapping_sub(1)))
                .ok_or(format!("no report {n} (see `luchctl reports`)"))?;
            let id = r["id"].as_str().unwrap_or("");
            match action.first().copied() {
                None => println!("{}", serde_json::to_string_pretty(r).unwrap_or_default()),
                Some("open") => {
                    let url = p.report_url(id).await.map_err(e)?;
                    if std::process::Command::new("xdg-open")
                        .arg(&url)
                        .spawn()
                        .is_err()
                    {
                        println!("{url}");
                    }
                }
                Some("dismiss") => p.dismiss_report(id).await.map_err(e)?,
                Some(other) => return Err(format!("report {n} open|dismiss, not {other}")),
            }
        }
        ["known"] => {
            let (all, _) = known(&p, None).await?;
            if all.is_empty() {
                println!("(none yet: luchd remembers a receiver once it has connected to it)");
            }
            for r in &all {
                let wake: Vec<&str> = r["wake"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| m.as_str())
                    .collect();
                println!(
                    "{:<38} {:<20} {:<16} {:<8} wake: {}",
                    r["id"].as_str().unwrap_or(""),
                    r["name"].as_str().unwrap_or(""),
                    r["model"].as_str().unwrap_or(""),
                    if r["online"] == true { "on" } else { "off" },
                    if wake.is_empty() {
                        "-".to_string()
                    } else {
                        wake.join(", ")
                    }
                );
                if let Some(h) = r["hint"].as_str() {
                    println!("{:<38} {h}", "");
                }
            }
        }
        ["wake", key @ ..] if key.len() <= 1 => {
            let (all, one) = known(&p, key.first().copied()).await?;
            let r = need_one(one, &all)?;
            let (id, name) = (
                r["id"].as_str().unwrap_or(""),
                r["name"].as_str().unwrap_or(""),
            );
            if r["online"] == true {
                println!("{name} is already on");
                return Ok(());
            }
            let mut changes = p.receive_receivers_changed().await.map_err(e)?;
            let how = p.wake(id).await.map_err(e)?;
            println!("waking {name} ({how}), waiting for it on the LAN…");
            let up = tokio::time::timeout(std::time::Duration::from_secs(90), async {
                loop {
                    if known(&p, Some(id))
                        .await
                        .is_ok_and(|(_, one)| one.is_some_and(|r| r["online"] == true))
                    {
                        return;
                    }
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), changes.next())
                        .await;
                    let _ = p.refresh().await;
                }
            })
            .await;
            match up {
                Ok(()) => println!("{name} is on"),
                Err(_) => return Err(format!("{name} did not show up in 90 s")),
            }
        }
        ["wake", key, "learn"] => {
            let (all, one) = known(&p, Some(key)).await?;
            let r = need_one(one, &all)?;
            println!("listening 30 s: with {} off, press the power key on its remote near this computer…", r["name"].as_str().unwrap_or(key));
            let adv = p
                .learn_wake(r["id"].as_str().unwrap_or(""), 30)
                .await
                .map_err(e)?;
            println!("learned {adv}; `luchctl wake {key}` turns it on now");
        }
        ["wake", key, kind @ ("mac" | "ble"), value] => {
            let (all, one) = known(&p, Some(key)).await?;
            let r = need_one(one, &all)?;
            let value = if *value == "off" { "" } else { value };
            p.set_wake(r["id"].as_str().unwrap_or(""), kind, value)
                .await
                .map_err(e)?;
        }
        ["watch"] => {
            let mut changes = p.receive_receivers_changed().await.map_err(e)?;
            p.refresh().await.map_err(e)?;
            print(&p.receivers().await.map_err(e)?);
            while changes.next().await.is_some() {
                println!("--- changed");
                print(&p.receivers().await.map_err(e)?);
            }
        }
        ["browser", "install"] => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
            let done = native::install(&exe.to_string_lossy(), &home).map_err(|e| e.to_string())?;
            if done.is_empty() {
                println!("no Chrome, Chromium, Brave, Edge, Yandex or Firefox profile found");
            }
            for f in done {
                println!("{f}");
            }
            println!(
                "now add the Luch extension to the browser (browser-extension/ in this repository)"
            );
        }
        [] => tui(&p).await.map_err(e)?,
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

const MEDIA_WORDS: [&str; 12] = [
    "play", "pause", "toggle", "seek", "skip", "rate", "speed", "audio", "sub", "state", "watch",
    "off",
];

fn looks_like_item(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://") || std::path::Path::new(s).exists()
}

/// Items and the Cast options JSON from `cast` arguments.
fn cast_args(args: &[&str]) -> Result<(Vec<String>, serde_json::Value), String> {
    let mut items = Vec::new();
    let mut o = serde_json::json!({});
    let mut subs = Vec::new();
    let mut it = args.iter().copied();
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or(format!("{a} needs a value"));
        match a {
            "--title" => o["title"] = value()?.into(),
            "--start" => o["start"] = parse_time(value()?)?.into(),
            "--sub" => subs.push(absolute(value()?)),
            "--relay" => o["via"] = "sender".into(),
            "--paused" => o["paused"] = true.into(),
            a if a.starts_with("--") => return Err(format!("unknown option {a}")),
            a => items.push(absolute(a)),
        }
    }
    if items.is_empty() {
        return Err("cast what? a file or a link".into());
    }
    if !subs.is_empty() {
        o["subs"] = subs.into();
    }
    Ok((items, o))
}

/// A path as luchd sees it (it runs elsewhere): absolute; links stay as they are.
fn absolute(s: &str) -> String {
    if s.contains("://") {
        return s.to_string();
    }
    std::fs::canonicalize(s)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| s.to_string())
}

/// "90", "1:30" or "1:02:03" in seconds.
fn parse_time(s: &str) -> Result<f64, String> {
    let mut total = 0.0;
    for part in s.split(':') {
        total = total * 60.0
            + part
                .parse::<f64>()
                .map_err(|_| format!("not a time: {s}"))?;
    }
    Ok(total)
}

/// The media_cmd for `media` words (PROTOCOL.md 4b).
fn media_command(words: &[&str]) -> Result<serde_json::Value, String> {
    use serde_json::json;
    let num = |w: Option<&&str>| -> Result<f64, String> {
        w.ok_or("a number is missing")?
            .parse::<f64>()
            .map_err(|_| "not a number".to_string())
    };
    Ok(match words {
        [w @ ("play" | "pause" | "toggle" | "state")] => json!({"do": w}),
        ["seek", t] => json!({"do": "seek", "pos": parse_time(t)?}),
        ["skip", rest @ ..] => json!({"do": "skip", "by": num(rest.first())?}),
        ["rate" | "speed", rest @ ..] => json!({"do": "rate", "rate": num(rest.first())?}),
        ["audio", id] => json!({"do": "audio", "id": id}),
        ["sub", "off"] => json!({"do": "sub", "id": null}),
        ["sub", id] => json!({"do": "sub", "id": id}),
        _ => return Err(USAGE.into()),
    })
}

fn clock(s: f64) -> String {
    let s = s.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// One human line from a media_state.
fn state_line(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let Some(state) = v["state"].as_str() else {
        return "(nothing playing)".into();
    };
    let mut line = state.to_string();
    if let Some(pos) = v["pos"].as_f64() {
        line += &format!(" {}", clock(pos));
        if let Some(d) = v["dur"].as_f64() {
            line += &format!(" / {}", clock(d));
        }
    }
    if let Some(r) = v["rate"].as_f64().filter(|r| (*r - 1.0).abs() > 0.001) {
        line += &format!(" · {r}x");
    }
    if let (Some(w), Some(h)) = (v["w"].as_u64(), v["h"].as_u64()) {
        line += &format!(" · {w}x{h}");
    }
    for (kind, word) in [("audio", "sound"), ("sub", "subtitles")] {
        let list: Vec<String> = v["tracks"][kind]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| {
                let id = t["id"].as_str().unwrap_or("");
                let mark = if v["sel"][kind] == id { "*" } else { "" };
                format!("{mark}{id} {}", t["label"].as_str().unwrap_or(""))
            })
            .collect();
        if !list.is_empty() {
            line += &format!(" · {word}: {}", list.join(", "));
        }
    }
    if v["by"] == "receiver" {
        line += " (on the TV)";
    }
    if let Some(d) = v["detail"].as_str() {
        line += &format!(" · {d}");
    }
    line
}

/// One human line from the daemon's Stats JSON.
fn stats_line(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let (s, r) = (&v["sender"], &v["receiver"]);
    if s.is_null() {
        return "(not mirroring)".into();
    }
    let n = |x: &serde_json::Value| x.as_f64().map(|f| format!("{f}")).unwrap_or("–".into());
    format!(
        "≈{} ms total | rtt {} ms · host {} ms · decode {} ms | send {} fps {} Mbit/s · shown {} fps · lost {} dropped {}",
        n(&v["est_ms"]), n(&s["rtt_ms"]), n(&s["host_ms"]), n(&r["decode_ms"]),
        n(&s["fps"]), n(&s["mbps"]), n(&r["fps"]), n(&r["lost"]), n(&r["dropped"])
    )
}

fn draw(rows: &[ReceiverRow], stats: &[(usize, String)], note: &str) {
    print!("\x1b[2J\x1b[H"); // clear screen, cursor home
    println!("Luch — receivers on the LAN\n");
    if rows.is_empty() {
        println!("  (searching…)");
    }
    for (i, (_, name, model, addr, ifname, port, connected, mirroring)) in rows.iter().enumerate() {
        let mark = if *mirroring {
            "▶"
        } else if *connected {
            "●"
        } else {
            "○"
        };
        println!(
            "  {} {mark} {name:<24} {model:<14} {addr}:{port} ({ifname})",
            i + 1
        );
        if let Some((_, line)) = stats.iter().find(|(j, _)| *j == i) {
            println!("      {line}");
        }
    }
    println!("\n  <N> connect/disconnect · f<N> fast full screen · m<N> mirror screen · w<N> one window · e<N> extend · s<N> stop · r refresh · q quit");
    if !note.is_empty() {
        println!("  {note}");
    }
    print!("> ");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

// ponytail: line-based screen, no raw mode; ratatui when it grows menus.
async fn tui(p: &Luch1Proxy<'_>) -> zbus::Result<()> {
    use tokio::io::AsyncBufReadExt;
    let mut changes = p.receive_receivers_changed().await?;
    p.refresh().await?;
    let mut input = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut rows = p.receivers().await?;
    let mut note = String::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        let mut stats = Vec::new();
        for (i, r) in rows.iter().enumerate().filter(|(_, r)| r.7) {
            stats.push((i, stats_line(&p.stats(&r.0).await?)));
        }
        draw(&rows, &stats, &note);
        tokio::select! {
            _ = changes.next() => {}
            _ = tick.tick() => {}
            line = input.next_line() => {
                let Ok(Some(line)) = line else { break };
                let cmd = line.trim();
                note.clear();
                let pick = |n: &str| n.parse::<usize>().ok().filter(|n| (1..=rows.len()).contains(n)).map(|n| &rows[n - 1]);
                let action = cmd.get(..1).zip(cmd.get(1..)).and_then(|(a, n)| Some((a, pick(n)?)));
                if let Some((a @ ("f" | "m" | "w" | "e" | "s"), (id, name, ..))) = action {
                    let r = match a {
                        "s" => p.stop_mirror(id).await.map(|_| "stopped"),
                        "f" => p.mirror(id, "fast").await.map(|_| "mirroring full screen, lowest latency"),
                        "w" => p.mirror(id, "window").await.map(|_| "mirroring a window"),
                        "e" => p.mirror(id, "virtual").await.map(|_| "extending the desktop"),
                        _ => p.mirror(id, "monitor").await.map(|_| "mirroring"),
                    };
                    note = match r { Ok(s) => format!("{name}: {s}"), Err(err) => format!("{name}: {err}") };
                    rows = p.receivers().await?;
                    continue;
                }
                match (cmd, cmd.parse::<usize>()) {
                    ("q", _) => break,
                    (_, Ok(n)) if (1..=rows.len()).contains(&n) => {
                        let (id, name, _, _, _, _, connected, _) = &rows[n - 1];
                        let r: zbus::Result<&str> = if *connected { p.disconnect(id).await.map(|_| "disconnected") } else { p.connect(id).await.map(|_| "connected") };
                        note = match r {
                            Ok(s) => format!("{name}: {s}"),
                            Err(err) => format!("{name}: {err}"),
                        };
                    }
                    ("" | "r", _) => {
                        note = match p.refresh().await {
                            Ok(()) => "refreshing receivers…".into(),
                            Err(e) => e.to_string(),
                        };
                    }
                    _ => note = format!("unknown command: {cmd}"),
                }
            }
        }
        rows = p.receivers().await?;
    }
    println!();
    Ok(())
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Started by a browser for the extension: Chromium passes the extension's origin, Firefox the manifest's path
    // and the extension's id.
    if args
        .first()
        .is_some_and(|a| a.starts_with("chrome-extension://"))
        || args.get(1).is_some_and(|a| a == "luch@toxblh.github.io")
    {
        let r = async {
            let conn = zbus::Connection::session()
                .await
                .map_err(|e| e.to_string())?;
            let p = Luch1Proxy::new(&conn).await.map_err(|e| e.to_string())?;
            native::run(p).await
        };
        if let Err(msg) = r.await {
            eprintln!("luchctl native host: {msg}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(msg) = run(&args).await {
        eprintln!("luchctl: {msg}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_arguments() {
        let (items, o) = cast_args(&[
            "https://x.example/a.m3u8",
            "--start",
            "1:30",
            "--relay",
            "--title",
            "A",
        ])
        .unwrap();
        assert_eq!(items, vec!["https://x.example/a.m3u8"]);
        assert_eq!(o["start"], 90.0);
        assert_eq!(o["via"], "sender");
        assert_eq!(o["title"], "A");
        assert!(cast_args(&["--paused"]).is_err());
        assert!(cast_args(&["a.mp4", "--title"]).is_err());
    }

    #[test]
    fn media_words() {
        assert_eq!(media_command(&["seek", "2:00"]).unwrap()["pos"], 120.0);
        assert_eq!(
            media_command(&["sub", "off"]).unwrap()["id"],
            serde_json::Value::Null
        );
        assert!(media_command(&["fly"]).is_err());
    }

    #[test]
    fn state_lines() {
        let s = r#"{"state":"playing","pos":75.2,"dur":3600,"rate":1.5,"w":3840,"h":2160,"by":"receiver",
            "tracks":{"audio":[{"id":"a1","label":"English"},{"id":"a2","label":"Русский"}],"sub":[]},
            "sel":{"audio":"a2","sub":null}}"#;
        assert_eq!(state_line(s),
            "playing 1:15 / 1:00:00 · 1.5x · 3840x2160 · sound: a1 English, *a2 Русский (on the TV)");
        assert_eq!(state_line("{}"), "(nothing playing)");
    }
}
