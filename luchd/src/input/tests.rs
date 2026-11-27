//! The whole way on a private bus (`dbus-run-session`): datagrams on a UDP socket pair, the permission gate, the
//! tracker, the injector and ashpd's portal calls, against `fake_portal`.

use std::{net::UdpSocket, sync::Arc, time::Duration};

use ashpd::desktop::{remote_desktop::DeviceType, screencast::SourceType};

use super::{
    fake_portal::{self, Log, NODE},
    inject::Injector,
    map::Geometry,
    portal,
    wire::{encode, Contact, Keys, Packet, Pointer, Record, Tool},
    Answer, Input,
};

fn finger(id: u8, x: u16, y: u16) -> Contact {
    Contact {
        id,
        tool: Tool::Finger,
        button: false,
        hover: false,
        x,
        y,
        pressure: 0,
    }
}

/// The receiver's video socket and luchd's connected one, as in a session.
fn flow() -> (UdpSocket, UdpSocket) {
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    sender.connect(receiver.local_addr().unwrap()).unwrap();
    receiver.connect(sender.local_addr().unwrap()).unwrap();
    (sender, receiver)
}

async fn calls_after(log: &Log, from: usize, want: usize) -> Vec<String> {
    for _ in 0..100 {
        let calls = log.lock().unwrap()[from..].to_vec();
        if calls.len() >= want {
            return calls;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    log.lock().unwrap()[from..].to_vec()
}

fn len(log: &Log) -> usize {
    log.lock().unwrap().len()
}

#[tokio::test(flavor = "multi_thread")]
async fn portal_session_permission_and_injection() {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        eprintln!("no session bus: skipped");
        return;
    }
    let (_portal, log) = fake_portal::serve(7).await;
    let dir = std::env::temp_dir().join(format!("luch-input-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // One RemoteDesktop session that carries the screencast: devices, sources, start, the PipeWire remote.
    let (remote, _fd) = portal::open(SourceType::MONITOR, &dir).await.unwrap();
    assert_eq!(
        log.lock().unwrap().clone(),
        [
            "CreateSession",
            "SelectDevices types=7 persist=2 restore=None",
            "SelectSources types=1",
            "Start",
            "OpenPipeWireRemote"
        ]
    );
    assert_eq!(remote.node, NODE);
    assert_eq!(
        (remote.position, remote.size),
        (Some((1920, 0)), Some((1920, 1080)))
    );
    assert_eq!(
        remote.devices,
        DeviceType::KEYBOARD | DeviceType::POINTER | DeviceType::TOUCHSCREEN
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("restore-token-remote")).unwrap(),
        "token-from-the-fake"
    );

    // The next session restores with the token: no dialog.
    let mark = len(&log);
    let (pointer_only, _fd2) = portal::open(SourceType::MONITOR, &dir).await.unwrap();
    assert_eq!(
        log.lock().unwrap()[mark + 1],
        "SelectDevices types=7 persist=2 restore=Some(\"token-from-the-fake\")"
    );

    let remote = Arc::new(remote);
    let geo = Geometry::from_stream(remote.position, remote.size, (1920, 1080), None);
    let (video, rx) = flow();
    let input = Input::start(
        &video,
        Injector::new(Some(remote.clone()), false, geo),
        Default::default(),
    )
    .unwrap();
    let grant = input.message(Answer::Once);
    assert_eq!(grant["grant"], "once");
    assert_eq!(
        grant["accept"],
        serde_json::json!(["touch", "pointer", "keyboard"])
    );
    assert_eq!(grant["area"], serde_json::json!([1920, 1080]));
    assert_eq!(grant["token"].as_str().unwrap().len(), 16);
    assert_eq!(
        input.message(Answer::No),
        serde_json::json!({"t": "input", "grant": "no"})
    );

    let send = |seq: u32, token: u64, records: Vec<Record>| {
        rx.send(&encode(&Packet {
            seq,
            time_us: 0,
            token,
            records,
        }))
        .unwrap();
    };

    // Before the grant nothing is injected.
    let mark = len(&log);
    send(1, input.token, vec![Record::Touch(vec![finger(0, 0, 0)])]);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(len(&log), mark, "no input before the grant");

    input.grant(true);
    // A wrong token is dropped; the right one lands at stream coordinates of node 42.
    send(
        2,
        input.token ^ 1,
        vec![Record::Touch(vec![finger(0, 65535, 65535)])],
    );
    send(
        3,
        input.token,
        vec![Record::Touch(vec![
            finger(0, 32768, 16384),
            finger(1, 65535, 0),
        ])],
    );
    send(
        4,
        input.token,
        vec![Record::Touch(vec![finger(1, 0, 65535)])],
    );
    let calls = calls_after(&log, mark, 4).await;
    let x0 = 32768.0 / 65535.0 * 1920.0;
    let y0 = 16384.0 / 65535.0 * 1080.0;
    assert_eq!(
        calls,
        [
            format!("TouchDown stream=42 slot=0 {x0} {y0}"),
            "TouchDown stream=42 slot=1 1920 0".to_string(),
            "TouchUp slot=0".to_string(),
            "TouchMotion stream=42 slot=1 0 1080".to_string(),
        ]
    );

    // Pointer: the first record is the base; then motion, a click, a wheel notch, a smooth scroll and its end.
    let mark = len(&log);
    let p = |rel, buttons, wheel, smooth| {
        Record::Pointer(Pointer {
            buttons,
            abs: None,
            smooth,
            rel,
            wheel,
        })
    };
    send(
        5,
        input.token,
        vec![Record::Touch(vec![]), p((0, 0), 0, (0, 0), false)],
    );
    send(6, input.token, vec![p((80, -16), 1, (120, 0), false)]);
    send(7, input.token, vec![p((80, -16), 0, (180, 0), true)]);
    send(8, input.token, vec![p((80, -16), 0, (180, 0), true)]);
    let calls = calls_after(&log, mark, 7).await;
    assert_eq!(
        calls,
        [
            "TouchUp slot=1",
            "PointerMotion 5 -1",
            "PointerAxisDiscrete axis=0 1",
            "PointerButton 0x110 1",
            "PointerAxis 0 7.5 finish=false",
            "PointerButton 0x110 0",
            "PointerAxis 0 0 finish=true",
        ]
    );
    // Keys: shift and A; a key still held when the receiver falls silent is let go by the watchdog.
    let mark = len(&log);
    send(
        9,
        input.token,
        vec![Record::Keys(Keys {
            first: 0,
            events: vec![(42, true), (30, true), (30, false)],
            held: vec![42],
        })],
    );
    let calls = calls_after(&log, mark, 4).await;
    assert_eq!(
        calls,
        [
            "KeyboardKeycode 42 1",
            "KeyboardKeycode 30 1",
            "KeyboardKeycode 30 0",
            "KeyboardKeycode 42 0"
        ]
    );

    // Taken back during the session: nothing more is injected.
    input.grant(false);
    let mark = len(&log);
    send(10, input.token, vec![Record::Touch(vec![finger(3, 1, 1)])]);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(len(&log), mark);
    drop(input);

    // A desktop that gives the pointer but no touch screen: the first finger is the pointer.
    let mut pointer_only = pointer_only;
    pointer_only.devices = DeviceType::POINTER;
    let (video, rx) = flow();
    let input = Input::start(
        &video,
        Injector::new(Some(Arc::new(pointer_only)), false, geo),
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        input.message(Answer::Always)["accept"],
        serde_json::json!(["touch", "pointer"])
    );
    input.grant(true);
    let mark = len(&log);
    let send = |seq: u32, records: Vec<Record>| {
        rx.send(&encode(&Packet {
            seq,
            time_us: 0,
            token: input.token,
            records,
        }))
        .unwrap()
    };
    send(
        1,
        vec![Record::Touch(vec![
            finger(5, 0, 0),
            finger(6, 65535, 65535),
        ])],
    );
    send(2, vec![Record::Touch(vec![])]);
    let calls = calls_after(&log, mark, 3).await;
    assert_eq!(
        calls,
        [
            "PointerMotionAbsolute stream=42 0 0",
            "PointerButton 0x110 1",
            "PointerButton 0x110 0"
        ]
    );

    remote.session.close().await.unwrap();
    assert!(log.lock().unwrap().iter().any(|c| c == "Session.Close"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_permission_steps() {
    use super::Step;
    let geo = Geometry::alone(1920.0, 1080.0);
    let (video, _rx) = flow();
    // uinput stands in for "something can be injected" without a portal.
    let mut i = Input::start(&video, Injector::new(None, true, geo), Default::default()).unwrap();
    assert_eq!(i.remembered(false), None);
    assert_eq!(i.request(), Step::Ask);
    assert_eq!(i.request(), Step::Wait, "one question at a time");
    let m = i.answer(Answer::Once);
    assert_eq!(m["grant"], "once");
    assert_eq!(
        m["accept"],
        serde_json::json!(["touch", "pointer", "keyboard", "gamepad", "pen"])
    );
    assert!(i.granted.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(
        i.request(),
        Step::Reply(m),
        "asked again in the session: the same answer"
    );

    let mut i = Input::start(&video, Injector::new(None, true, geo), Default::default()).unwrap();
    assert_eq!(i.request(), Step::Ask);
    assert_eq!(
        i.answer(Answer::No),
        serde_json::json!({"t": "input", "grant": "no"})
    );
    assert!(!i.granted.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(
        i.request(),
        Step::Reply(serde_json::json!({"t": "input", "grant": "no"})),
        "no holds for the session"
    );

    // "Always" before: granted at the start, no question.
    let mut i = Input::start(&video, Injector::new(None, true, geo), Default::default()).unwrap();
    assert_eq!(i.remembered(true).unwrap()["grant"], "always");
    assert!(matches!(i.request(), Step::Reply(m) if m["grant"] == "always"));

    // Nothing to inject with: no, without asking, even after an "always".
    let mut i = Input::start(&video, Injector::new(None, false, geo), Default::default()).unwrap();
    assert_eq!(i.remembered(true), None);
    assert_eq!(
        i.request(),
        Step::Reply(serde_json::json!({"t": "input", "grant": "no"}))
    );
}

/// A receiver without sound sends its clock requests on the video flow too (PROTOCOL.md 4k): the input reader, the
/// socket's only reader, answers them once the pts clock is there.
#[tokio::test]
async fn clock_requests_on_the_input_socket_are_answered() {
    let geo = Geometry::alone(1920.0, 1080.0);
    let (video, rx) = flow();
    rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let clock: Arc<std::sync::OnceLock<crate::sync::Now>> = Arc::default();
    let _i = Input::start(&video, Injector::new(None, false, geo), clock.clone()).unwrap();
    let _ = clock.set(Arc::new(|| Some(1_000_000)));
    let mut req = vec![0u8; 32];
    req[0] = crate::sync::KIND_CLOCK;
    req[8..12].copy_from_slice(&9u32.to_be_bytes());
    req[16..20].copy_from_slice(&8u32.to_be_bytes());
    req[24..32].copy_from_slice(&0xabcdefu64.to_be_bytes());
    rx.send(&req).unwrap();
    let mut buf = [0u8; 64];
    let n = rx.recv(&mut buf).unwrap();
    assert_eq!(n, 48);
    assert_eq!(buf[1], crate::sync::F_REPLY);
    assert_eq!(
        u64::from_be_bytes(buf[32..40].try_into().unwrap()),
        1_000_000
    );
}
