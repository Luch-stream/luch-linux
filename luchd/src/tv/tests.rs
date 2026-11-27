//! luchd against the fake TVs (fake_dlna.rs, fake_cast.rs): found the way real ones are, fed real files through
//! `cast::Server`, driven through the session and then through the D-Bus API itself.

use std::{net::IpAddr, path::PathBuf};

use serde_json::{json, Value};
use tokio::{net::TcpListener, sync::mpsc, time::timeout};

use super::{fake_cast::FakeCast, fake_dlna::FakeRenderer, *};
use crate::cast;

const LOOPBACK: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

async fn files() -> cast::Server {
    cast::Server::serve(TcpListener::bind("127.0.0.1:0").await.unwrap()).unwrap()
}

fn temp(name: &str, data: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luch-tv-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

fn video_and_subs() -> Vec<String> {
    let v = temp("Holiday 2026.mp4", &vec![7u8; 50_000]);
    let s = v.with_file_name("Holiday 2026.en.srt");
    std::fs::write(&s, "1\r\n00:00:01,000 --> 00:00:02,500\r\nHello\r\n").unwrap();
    vec![v.to_string_lossy().into(), s.to_string_lossy().into()]
}

/// Waits for a state matching `want`, failing on any end that does not.
async fn until(rx: &mut mpsc::UnboundedReceiver<Value>, want: impl Fn(&Value) -> bool) -> Value {
    let mut seen = Vec::new();
    let got = timeout(Duration::from_secs(15), async {
        loop {
            let v = rx.recv().await.expect("the session ended without it");
            if want(&v) {
                return v;
            }
            assert_ne!(v["state"], "stopped", "stopped early: {v}");
            seen.push(v);
        }
    })
    .await;
    got.unwrap_or_else(|_| panic!("no such state in time; told instead: {seen:?}"))
}

fn is(state: &'static str) -> impl Fn(&Value) -> bool {
    move |v| v["state"] == state
}

fn cast_txt(port: u16) -> Device {
    let txt = crate::txt_map(&[
        b"id=1c4f3a2b9d8e7f60a1b2c3d4e5f60718".to_vec(),
        b"md=Chromecast".to_vec(),
        b"fn=Living room TV".to_vec(),
        b"ca=4101".to_vec(),
    ]);
    mdns::device("127.0.0.1", port, &txt).unwrap()
}

fn entry(kind: Kind, id: &str, address: &str) -> Entry {
    Entry {
        kind,
        id: id.into(),
        name: format!("TV {id}"),
        model: String::new(),
        address: address.into(),
        ifname: String::new(),
        port: 1,
    }
}

#[test]
fn one_row_per_tv_with_the_best_way_first() {
    let groups = group([
        entry(Kind::Dlna, "dlna:a", "192.168.1.40"),
        entry(Kind::Cast, "cast:a", "192.168.1.40"),
        entry(Kind::Luch, "luch-a", "192.168.1.40"),
        // A speaker group at the TV's address is another Chromecast: its own row.
        entry(Kind::Cast, "cast:group", "192.168.1.40"),
        entry(Kind::Dlna, "dlna:b", "192.168.1.41"),
    ]);
    assert_eq!(groups.len(), 3, "{groups:?}");
    let tv = groups.iter().find(|g| g.ways[0].id == "luch-a").unwrap();
    let kinds: Vec<Kind> = tv.ways.iter().map(|w| w.kind).collect();
    assert_eq!(kinds, [Kind::Luch, Kind::Cast, Kind::Dlna]);
    assert_eq!(pick(&groups, "luch-a", None).unwrap().id, "luch-a");
    // The hidden choice: any id of the TV, any of its ways.
    assert_eq!(
        pick(&groups, "dlna:a", Some(Kind::Cast)).unwrap().id,
        "cast:a"
    );
    assert_eq!(
        pick(&groups, "luch-a", Some(Kind::Dlna)).unwrap().id,
        "dlna:a"
    );
    assert!(pick(&groups, "dlna:b", Some(Kind::Luch)).is_none());
    assert!(pick(&groups, "nobody", None).is_none());
    let d = describe(tv);
    assert_eq!(d["kind"], "luch");
    assert_eq!(d["transports"].as_array().unwrap().len(), 3);
    assert!(d["can"].as_array().unwrap().contains(&json!("mirror")));
    let lone = describe(groups.iter().find(|g| g.ways[0].id == "dlna:b").unwrap());
    assert_eq!(lone["kind"], "dlna");
    assert!(!lone["can"].as_array().unwrap().contains(&json!("mirror")));
}

#[tokio::test]
async fn what_a_tv_is_given_comes_from_the_luch_message() {
    let srv = files().await;
    let items = video_and_subs();
    let msg = cast::build(&items, &cast::Options::default(), &srv, LOOPBACK, LOOPBACK).unwrap();
    let m = Media::from_message(&msg).unwrap();
    assert_eq!(m.mime, "video/mp4");
    assert_eq!(m.title, "Holiday 2026");
    assert!(m.ours && !m.photo);
    assert_eq!(m.subs.len(), 1);
    assert_eq!(m.subs[0].lang, "en");
    let p = temp("a.jpg", b"jpeg");
    let photo = cast::build(
        &[p.to_string_lossy().into()],
        &cast::Options::default(),
        &srv,
        LOOPBACK,
        LOOPBACK,
    )
    .unwrap();
    assert!(Media::from_message(&photo).unwrap().photo);
    let yt = json!({"t": "open_app", "app": "youtube", "id": "x"});
    assert!(Media::from_message(&yt).is_err());
    assert!(Media::from_message(&json!({"t": "doc"})).is_err());
}

#[test]
fn states_are_told_on_change_not_on_every_poll() {
    let (_s, mut ends, mut rx) = session();
    ends.tell.tell("loading", 0.0, None, 1.0);
    ends.tell.tell("playing", 0.0, Some(60.0), 1.0);
    ends.tell.tell("playing", 0.3, Some(60.0), 1.0); // just time going on
    ends.tell.tell("playing", 30.0, Some(60.0), 1.0); // a seek on the TV
    ends.tell.asked();
    ends.tell.tell("paused", 30.0, Some(60.0), 1.0);
    ends.tell.asked();
    ends.tell.tell("paused", 30.0, Some(60.0), 1.0);
    ends.tell.stopped("receiver");
    let got: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let short: Vec<(String, f64, String)> = got
        .iter()
        .map(|v| {
            (
                v["state"].as_str().unwrap().into(),
                v["pos"].as_f64().unwrap_or(-1.0),
                v["by"].as_str().unwrap_or("").into(),
            )
        })
        .collect();
    assert_eq!(
        short,
        [
            ("loading".into(), 0.0, "receiver".into()),
            ("playing".into(), 0.0, "receiver".into()),
            ("playing".into(), 30.0, "receiver".into()),
            ("paused".into(), 30.0, "sender".into()),
            ("paused".into(), 30.0, "sender".into()),
            ("stopped".into(), -1.0, "".into()),
        ]
    );
    assert_eq!(got[1]["dur"], 60.0);
    assert_eq!(got[0]["live"], false);
    assert_eq!(got[5]["reason"], "receiver");
}

/// Found over real SSDP multicast (TTL 0: only this machine hears it), then played, paused, sought, its volume
/// set, played to the end.
#[tokio::test]
async fn a_dlna_tv_is_found_and_plays_to_the_end() {
    let fake = FakeRenderer::start(4.0).await;
    let answers = ssdp::search(
        ssdp::Target {
            to: ssdp::GROUP.into(),
            ttl: 0,
        },
        Duration::from_millis(1500),
    )
    .await
    .unwrap();
    let a = answers
        .iter()
        .find(|a| a.usn.starts_with(&fake.udn))
        .unwrap_or_else(|| {
            panic!("the fake renderer did not answer the multicast search: {answers:?}")
        });
    let http = reqwest::Client::new();
    let d = ssdp::describe(&http, &a.location).await.unwrap().unwrap();
    assert_eq!((d.kind, d.name.as_str()), (Kind::Dlna, "Living room TV"));

    let srv = files().await;
    let msg = cast::build(
        &video_and_subs(),
        &cast::Options::default(),
        &srv,
        LOOPBACK,
        LOOPBACK,
    )
    .unwrap();
    let m = Media::from_message(&msg).unwrap();
    dlna::prepare(&srv, &m);
    let (s, mut rx) = play(&d, m.clone(), &http).await.unwrap();
    until(&mut rx, is("playing")).await;
    {
        let av = fake.av.lock().unwrap();
        let f = &av.fetches[0];
        assert_eq!(f.status, 206, "a TV asks for a range: {f:?}");
        assert_eq!(f.headers["contentfeatures.dlna.org"], cast::DLNA_STREAM);
        assert_eq!(f.headers["transfermode.dlna.org"], "Streaming");
        assert_eq!(f.headers["captioninfo.sec"], m.subs[0].url);
        assert_eq!(f.headers["content-range"], "bytes 0-49999/50000");
        assert!(av.meta.contains("sec:CaptionInfoEx"));
        assert_eq!(av.uri, m.url);
    }
    let acts = fake.actions();
    assert_eq!(
        &acts[..3],
        ["Stop", "SetAVTransportURI", "Play"],
        "{acts:?}"
    );

    let cmd = |c: Value| s.ask(Ask::Cmd(c));
    cmd(json!({"do": "pause"})).await.unwrap();
    let p = until(&mut rx, is("paused")).await;
    assert_eq!(p["by"], "sender");
    cmd(json!({"do": "seek", "pos": 2})).await.unwrap();
    until(&mut rx, |v| v["state"] == "paused" && v["pos"] == 2.0).await;
    assert_eq!(
        cmd(json!({"do": "rate", "rate": 1.5})).await.unwrap_err(),
        "this TV plays at one speed only"
    );
    let vol = |v: Value| s.ask(Ask::Volume(v));
    assert_eq!(
        vol(json!({"t": "volume", "level": 40})).await.unwrap(),
        json!({"level": 40, "muted": false})
    );
    assert_eq!(
        vol(json!({"t": "volume", "step": 1})).await.unwrap(),
        json!({"level": 45, "muted": false})
    );
    assert_eq!(
        vol(json!({"t": "volume", "muted": true})).await.unwrap(),
        json!({"level": 45, "muted": true})
    );
    cmd(json!({"do": "play"})).await.unwrap();
    until(&mut rx, is("ended")).await;
    let end = until(&mut rx, is("stopped")).await;
    assert_eq!(end["reason"], "ended");
}

#[tokio::test]
async fn a_dlna_tv_stopped_on_its_remote_or_by_us() {
    let fake = FakeRenderer::start(600.0).await;
    let http = reqwest::Client::new();
    let d = ssdp::describe(&http, &fake.location)
        .await
        .unwrap()
        .unwrap();
    let srv = files().await;
    let msg = cast::build(
        &video_and_subs()[..1],
        &cast::Options {
            start: 30.0,
            ..Default::default()
        },
        &srv,
        LOOPBACK,
        LOOPBACK,
    )
    .unwrap();
    let (_s, mut rx) = play(&d, Media::from_message(&msg).unwrap(), &http)
        .await
        .unwrap();
    // The start position is sought once it plays.
    until(&mut rx, |v| {
        v["state"] == "playing" && v["pos"].as_f64().unwrap() >= 30.0
    })
    .await;
    fake.press_stop();
    assert_eq!(until(&mut rx, is("stopped")).await["reason"], "receiver");

    let (s, mut rx) = play(&d, Media::from_message(&msg).unwrap(), &http)
        .await
        .unwrap();
    until(&mut rx, is("playing")).await;
    s.ask(Ask::Stop).await.unwrap();
    assert_eq!(until(&mut rx, is("stopped")).await["reason"], "sender");
    assert_eq!(fake.av.lock().unwrap().state, "STOPPED");
}

#[tokio::test]
async fn a_chromecast_plays_with_subtitles_and_obeys() {
    let fake = FakeCast::start(30.0).await;
    let d = cast_txt(fake.port);
    let srv = files().await;
    let msg = cast::build(
        &video_and_subs(),
        &cast::Options::default(),
        &srv,
        LOOPBACK,
        LOOPBACK,
    )
    .unwrap();
    let mut m = Media::from_message(&msg).unwrap();
    castv2::prepare(&srv, &mut m);
    assert_eq!(
        m.subs[0].mime, "text/vtt",
        "SubRip is served as WebVTT to a Chromecast"
    );
    let (s, mut rx) = play(&d, m, &reqwest::Client::new()).await.unwrap();
    until(&mut rx, is("playing")).await;
    {
        let tv = fake.tv.lock().unwrap();
        let log = tv.log.join(", ");
        assert!(
            log.starts_with("connection CONNECT, receiver LAUNCH, connection CONNECT, media LOAD"),
            "{log}"
        );
        assert_eq!(
            tv.tracks,
            [("text/vtt; charset=utf-8".to_string(), true)],
            "WebVTT with CORS"
        );
        assert_eq!(tv.active_tracks, [1]);
    }
    let cmd = |c: Value| s.ask(Ask::Cmd(c));
    cmd(json!({"do": "pause"})).await.unwrap();
    assert_eq!(until(&mut rx, is("paused")).await["by"], "sender");
    cmd(json!({"do": "seek", "pos": 1})).await.unwrap();
    until(&mut rx, |v| v["state"] == "paused" && v["pos"] == 1.0).await;
    cmd(json!({"do": "sub", "id": null})).await.unwrap();
    assert!(fake.tv.lock().unwrap().active_tracks.is_empty());
    cmd(json!({"do": "rate", "rate": 2.0})).await.unwrap();
    until(&mut rx, |v| v["rate"] == 2.0).await;
    assert!(cmd(json!({"do": "audio", "id": "a1"})).await.is_err());
    let vol = |v: Value| s.ask(Ask::Volume(v));
    assert_eq!(
        vol(json!({"t": "volume", "level": 40})).await.unwrap(),
        json!({"level": 40, "muted": false})
    );
    assert_eq!(
        vol(json!({"t": "volume", "step": 1})).await.unwrap(),
        json!({"level": 45, "muted": false})
    );
    assert!((fake.tv.lock().unwrap().volume - 0.45).abs() < 1e-9);
    cmd(json!({"do": "play"})).await.unwrap();
    until(&mut rx, is("playing")).await;
    // Paused with the TV's remote: told as the receiver's doing.
    tokio::time::sleep(Duration::from_secs(3)).await;
    fake.press_pause();
    assert_eq!(until(&mut rx, is("paused")).await["by"], "receiver");
    cmd(json!({"do": "seek", "pos": 28})).await.unwrap();
    cmd(json!({"do": "play"})).await.unwrap();
    until(&mut rx, is("ended")).await;
    assert_eq!(until(&mut rx, is("stopped")).await["reason"], "ended");
}

#[tokio::test]
async fn a_chromecast_session_ends_when_stopped_or_taken_over() {
    let fake = FakeCast::start(600.0).await;
    let d = cast_txt(fake.port);
    let srv = files().await;
    let msg = cast::build(
        &video_and_subs()[..1],
        &cast::Options::default(),
        &srv,
        LOOPBACK,
        LOOPBACK,
    )
    .unwrap();
    let http = reqwest::Client::new();
    let (s, mut rx) = play(&d, Media::from_message(&msg).unwrap(), &http)
        .await
        .unwrap();
    until(&mut rx, is("playing")).await;
    s.ask(Ask::Stop).await.unwrap();
    assert_eq!(until(&mut rx, is("stopped")).await["reason"], "sender");
    assert!(
        fake.tv.lock().unwrap().app.is_none(),
        "our app is closed on the TV"
    );

    let (_s, mut rx) = play(&d, Media::from_message(&msg).unwrap(), &http)
        .await
        .unwrap();
    until(&mut rx, is("playing")).await;
    fake.take_over();
    assert_eq!(until(&mut rx, is("stopped")).await["reason"], "replaced");
}

#[tokio::test]
async fn a_chromecast_refuses_what_it_cannot_fetch() {
    let fake = FakeCast::start(60.0).await;
    let d = cast_txt(fake.port);
    let m = Media {
        url: "http://127.0.0.1:9/nothing.mp4".into(),
        mime: "video/mp4".into(),
        ..Default::default()
    };
    let e = play(&d, m, &reqwest::Client::new()).await.err().unwrap();
    assert!(e.contains("LOAD_FAILED"), "{e}");
}

/// A Cast receiver drops a sender that stays silent; a session that lasts longer than its timeout must keep
/// sending PINGs and answering the TV's.
#[tokio::test]
async fn a_chromecast_session_keeps_its_heartbeat() {
    let fake = FakeCast::start(600.0).await;
    let d = cast_txt(fake.port);
    let srv = files().await;
    let msg = cast::build(
        &video_and_subs()[..1],
        &cast::Options::default(),
        &srv,
        LOOPBACK,
        LOOPBACK,
    )
    .unwrap();
    let (s, mut rx) = play(
        &d,
        Media::from_message(&msg).unwrap(),
        &reqwest::Client::new(),
    )
    .await
    .unwrap();
    until(&mut rx, is("playing")).await;
    tokio::time::sleep(Duration::from_secs(11)).await;
    assert!(fake.tv.lock().unwrap().pings_from_sender >= 2);
    assert_eq!(s.state()["state"], "playing");
    s.ask(Ask::Cmd(json!({"do": "state"}))).await.unwrap();
    assert!(rx.try_recv().is_ok() || until(&mut rx, is("playing")).await["state"] == "playing");
}

// ---------- through D-Bus ----------

/// The next MediaStateChanged with state `want`.
async fn next_signal(s: &mut zbus::MessageStream, want: &'static str) -> Value {
    use futures_util::StreamExt;
    timeout(Duration::from_secs(15), async {
        loop {
            let m = s.next().await.unwrap().unwrap();
            let (_, state): (String, String) = m.body().deserialize().unwrap();
            let v: Value = serde_json::from_str(&state).unwrap();
            if v["state"] == want {
                return v;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {want} signal"))
}

/// The whole way a front-end goes: both fakes at one address are one row with two transports; Cast plays on the
/// best (Cast), the hidden choice on the other (DLNA); MediaState, MediaCmd, SetVolume and StopCast work on both,
/// MediaStateChanged tells, and Mirror says why not.
#[tokio::test]
async fn tvs_play_through_the_dbus_api() {
    use zbus::Connection;
    let Ok(client) = Connection::session().await else {
        eprintln!("no session bus: skipped");
        return;
    };
    let dlna_tv = FakeRenderer::start(600.0).await;
    let cast_tv = FakeCast::start(600.0).await;
    let tvs = Tvs::new();
    let http = reqwest::Client::new();
    tvs.saw(
        ssdp::describe(&http, &dlna_tv.location)
            .await
            .unwrap()
            .unwrap(),
    );
    tvs.saw(cast_txt(cast_tv.port));
    let (discovery, _worker) = crate::discovery::Discovery::new();
    let luch = crate::Luch {
        st: crate::Shared::default(),
        sender_id: "test".into(),
        host: "Linux Laptop".into(),
        discovery,
        tvs: tvs.clone(),
        http,
    };
    let server = zbus::connection::Builder::session()
        .unwrap()
        .serve_at(luch_proto::DBUS_PATH, luch)
        .unwrap()
        .build()
        .await
        .unwrap();
    let dest = server.unique_name().unwrap().to_owned();
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .member("MediaStateChanged")
        .unwrap()
        .build();
    let mut signals = zbus::MessageStream::for_match_rule(rule, &client, None)
        .await
        .unwrap();
    macro_rules! call {
        ($m:expr, $args:expr) => {
            client
                .call_method(
                    Some(dest.as_str()),
                    luch_proto::DBUS_PATH,
                    Some(luch_proto::DBUS_NAME),
                    $m,
                    $args,
                )
                .await
        };
    }
    let rows: Vec<luch_proto::ReceiverRow> = call!("Receivers", &())
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert_eq!(rows.len(), 1, "one TV, one row: {rows:?}");
    let id = rows[0].0.clone();
    assert!(id.starts_with("cast:"), "Cast ranks above DLNA: {rows:?}");
    assert_eq!(rows[0].1, "Living room TV");
    let devices: String = call!("Devices", &()).unwrap().body().deserialize().unwrap();
    let devices: Value = serde_json::from_str(&devices).unwrap();
    assert_eq!(devices[0]["kind"], "cast");
    assert_eq!(devices[0]["transports"][1]["kind"], "dlna");
    let caps: String = call!("Capabilities", &(id.as_str()))
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert!(caps.contains("\"media\""), "{caps}");

    let items = video_and_subs();
    let kind: String = call!(
        "Cast",
        &(id.as_str(), &items[..1], r#"{"title":"Holiday"}"#)
    )
    .unwrap()
    .body()
    .deserialize()
    .unwrap();
    assert_eq!(kind, "media");
    next_signal(&mut signals, "playing").await;
    assert!(cast_tv
        .tv
        .lock()
        .unwrap()
        .log
        .iter()
        .any(|l| l == "media LOAD"));
    let state: String = call!("MediaState", &(id.as_str()))
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert!(state.contains("\"playing\""), "{state}");
    call!("MediaCmd", &(id.as_str(), r#"{"do":"pause"}"#)).unwrap();
    next_signal(&mut signals, "paused").await;
    let vol: String = call!("SetVolume", &(id.as_str(), "40"))
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&vol).unwrap()["level"], 40);
    let mirror = call!("Mirror", &(id.as_str(), "monitor"))
        .unwrap_err()
        .to_string();
    assert!(mirror.contains("no Luch app"), "{mirror}");
    call!("StopCast", &(id.as_str())).unwrap();
    assert_eq!(
        next_signal(&mut signals, "stopped").await["reason"],
        "sender"
    );

    // The hidden choice: the same row over DLNA.
    let kind: String = call!(
        "Cast",
        &(id.as_str(), &items[..1], r#"{"transport":"dlna"}"#)
    )
    .unwrap()
    .body()
    .deserialize()
    .unwrap();
    assert_eq!(kind, "media");
    next_signal(&mut signals, "playing").await;
    assert!(dlna_tv.actions().contains(&"SetAVTransportURI".to_string()));
    call!("MediaCmd", &(id.as_str(), r#"{"do":"seek","pos":120}"#)).unwrap();
    call!("StopCast", &(id.as_str())).unwrap();
    assert_eq!(
        next_signal(&mut signals, "stopped").await["reason"],
        "sender"
    );
    assert_eq!(dlna_tv.av.lock().unwrap().state, "STOPPED");
    let e = call!(
        "Cast",
        &(id.as_str(), &items[..1], r#"{"transport":"luch"}"#)
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("cannot be reached that way"), "{e}");
    let state: String = call!("MediaState", &(id.as_str()))
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert_eq!(state, "{}");
}

// ---------- against a real renderer ----------

/// A WAV of `secs` seconds of silence (8 kHz, 8-bit mono): something any renderer decodes.
fn silence(secs: u32) -> Vec<u8> {
    let n = 8000 * secs;
    let mut w = Vec::with_capacity(44 + n as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + n).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&[16, 0, 0, 0, 1, 0, 1, 0]);
    w.extend_from_slice(&8000u32.to_le_bytes());
    w.extend_from_slice(&8000u32.to_le_bytes());
    w.extend_from_slice(&[1, 0, 8, 0]);
    w.extend_from_slice(b"data");
    w.extend_from_slice(&n.to_le_bytes());
    w.resize(44 + n as usize, 128);
    w
}

/// luchd against gmediarender (gmrender-resurrect), a real UPnP renderer, found over SSDP like a TV. Run by
/// contrib/tv-test/gmediarender.sh in Docker, with its sinks faked: no sound comes out.
#[tokio::test]
#[ignore]
async fn a_real_renderer_plays_what_luchd_casts() {
    let name =
        std::env::var("LUCH_REAL_RENDERER").expect("LUCH_REAL_RENDERER: the renderer's name");
    let http = reqwest::Client::new();
    let mut found = None;
    for _ in 0..5 {
        for a in ssdp::search(
            ssdp::Target {
                to: ssdp::GROUP.into(),
                ttl: 0,
            },
            Duration::from_secs(2),
        )
        .await
        .unwrap()
        {
            if let Ok(Some(d)) = ssdp::describe(&http, &a.location).await {
                if d.name == name {
                    found = Some(d);
                }
            }
        }
        if found.is_some() {
            break;
        }
    }
    let d = found.expect("the renderer did not answer");
    eprintln!("found {} ({}) at {}:{}", d.name, d.model, d.address, d.port);
    let me = local_for(d.address).unwrap();
    let srv = cast::Server::serve(TcpListener::bind((me, 0)).await.unwrap()).unwrap();
    let wav = temp("Silence.wav", &silence(20));
    let msg = cast::build(
        &[wav.to_string_lossy().into()],
        &cast::Options::default(),
        &srv,
        d.address,
        me,
    )
    .unwrap();
    let started = std::time::Instant::now();
    let (s, mut rx) = play(&d, Media::from_message(&msg).unwrap(), &http)
        .await
        .unwrap();
    // A renderer says PLAYING at once; it plays once it knows how long the file is.
    let p = until(&mut rx, |v| v["state"] == "playing" && v["dur"].is_number()).await;
    eprintln!("playing after {} ms: {p}", started.elapsed().as_millis());
    let cmd = |c: Value| s.ask(Ask::Cmd(c));
    let t = std::time::Instant::now();
    cmd(json!({"do": "pause"})).await.unwrap();
    until(&mut rx, is("paused")).await;
    eprintln!("paused after {} ms", t.elapsed().as_millis());
    cmd(json!({"do": "seek", "pos": 15})).await.unwrap();
    until(&mut rx, |v| {
        v["pos"].as_f64().is_some_and(|p| (p - 15.0).abs() < 1.5)
    })
    .await;
    let v = s
        .ask(Ask::Volume(json!({"t": "volume", "level": 30})))
        .await
        .unwrap();
    assert_eq!(v["level"], 30);
    cmd(json!({"do": "play"})).await.unwrap();
    until(&mut rx, is("playing")).await;
    let end = until(&mut rx, is("stopped")).await;
    eprintln!("ended: {end}");
    assert_eq!(end["reason"], "ended");
}
