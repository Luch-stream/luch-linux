//! A fake xdg-desktop-portal (RemoteDesktop + ScreenCast) on the private bus of `dbus-run-session`: the real
//! portal code (ashpd, `portal::open`, the injector) talks to it, and it writes down every call.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use zbus::{
    message::Header,
    zvariant::{ObjectPath, OwnedFd, OwnedValue, Value},
    Connection,
};

pub type Log = Arc<Mutex<Vec<String>>>;

pub const NODE: u32 = 42;

#[derive(Clone)]
struct Shared {
    log: Log,
    conn: Arc<Mutex<Option<Connection>>>,
    devices: u32,
}

impl Shared {
    fn note(&self, s: String) {
        self.log.lock().unwrap().push(s);
    }

    /// Answers a request the way the portal does: a Response signal on the request's own path.
    fn respond(
        &self,
        hdr: &Header<'_>,
        options: &HashMap<String, OwnedValue>,
        results: HashMap<String, OwnedValue>,
    ) {
        let path = request_path(hdr, options);
        let conn = self.conn.lock().unwrap().clone().unwrap();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = conn
                .emit_signal(
                    None::<()>,
                    &path,
                    "org.freedesktop.portal.Request",
                    "Response",
                    &(0u32, results),
                )
                .await;
        });
    }
}

fn string(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    options
        .get(key)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| String::try_from(v).ok())
}

/// Where the portal puts a request or session object: its kind, the caller's unique name, the caller's token.
fn object(hdr: &Header<'_>, kind: &str, token: Option<String>) -> String {
    let sender = hdr
        .sender()
        .unwrap()
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    format!(
        "/org/freedesktop/portal/desktop/{kind}/{sender}/{}",
        token.unwrap_or_default()
    )
}

fn session_path(hdr: &Header<'_>, options: &HashMap<String, OwnedValue>) -> String {
    object(hdr, "session", string(options, "session_handle_token"))
}

fn ov<'a>(v: impl Into<Value<'a>>) -> OwnedValue {
    v.into().try_to_owned().unwrap()
}

struct FakeRemote(Shared);

#[zbus::interface(name = "org.freedesktop.portal.RemoteDesktop")]
impl FakeRemote {
    #[zbus(property)]
    fn version(&self) -> u32 {
        2
    }

    #[zbus(property)]
    fn available_device_types(&self) -> u32 {
        self.0.devices
    }

    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> zbus::zvariant::OwnedObjectPath {
        let session = session_path(&hdr, &options);
        server
            .at(session.as_str(), FakeSession(self.0.clone()))
            .await
            .unwrap();
        self.0.note("CreateSession".into());
        let mut r = HashMap::new();
        r.insert("session_handle".to_string(), ov(session.as_str()));
        self.0.respond(&hdr, &options, r);
        request_path(&hdr, &options)
    }

    async fn select_devices(
        &self,
        _session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::zvariant::OwnedObjectPath {
        let types = options
            .get("types")
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        let persist = options
            .get("persist_mode")
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        let restore = string(&options, "restore_token");
        self.0.note(format!(
            "SelectDevices types={types} persist={persist} restore={restore:?}"
        ));
        self.0.respond(&hdr, &options, HashMap::new());
        request_path(&hdr, &options)
    }

    async fn start(
        &self,
        _session: ObjectPath<'_>,
        _parent: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::zvariant::OwnedObjectPath {
        self.0.note("Start".into());
        let mut props: HashMap<String, Value> = HashMap::new();
        props.insert("position".into(), Value::from((1920i32, 0i32)));
        props.insert("size".into(), Value::from((1920i32, 1080i32)));
        props.insert("source_type".into(), Value::from(1u32));
        let streams = vec![(NODE, props)];
        let mut r = HashMap::new();
        r.insert("devices".to_string(), ov(self.0.devices));
        r.insert("streams".to_string(), ov(Value::from(streams)));
        r.insert("restore_token".to_string(), ov("token-from-the-fake"));
        self.0.respond(&hdr, &options, r);
        request_path(&hdr, &options)
    }

    fn notify_pointer_motion(
        &self,
        _s: ObjectPath<'_>,
        _o: HashMap<String, OwnedValue>,
        dx: f64,
        dy: f64,
    ) {
        self.0.note(format!("PointerMotion {dx} {dy}"));
    }

    fn notify_pointer_motion_absolute(
        &self,
        _s: ObjectPath<'_>,
        _o: HashMap<String, OwnedValue>,
        stream: u32,
        x: f64,
        y: f64,
    ) {
        self.0
            .note(format!("PointerMotionAbsolute stream={stream} {x} {y}"));
    }

    fn notify_pointer_button(
        &self,
        _s: ObjectPath<'_>,
        _o: HashMap<String, OwnedValue>,
        button: i32,
        state: u32,
    ) {
        self.0.note(format!("PointerButton {button:#x} {state}"));
    }

    fn notify_pointer_axis(
        &self,
        _s: ObjectPath<'_>,
        o: HashMap<String, OwnedValue>,
        dx: f64,
        dy: f64,
    ) {
        let finish = o
            .get("finish")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false);
        self.0
            .note(format!("PointerAxis {dx} {dy} finish={finish}"));
    }

    fn notify_pointer_axis_discrete(
        &self,
        _s: ObjectPath<'_>,
        _o: HashMap<String, OwnedValue>,
        axis: u32,
        steps: i32,
    ) {
        self.0
            .note(format!("PointerAxisDiscrete axis={axis} {steps}"));
    }

    fn notify_keyboard_keycode(
        &self,
        _s: ObjectPath<'_>,
        _o: HashMap<String, OwnedValue>,
        keycode: i32,
        state: u32,
    ) {
        self.0.note(format!("KeyboardKeycode {keycode} {state}"));
    }

    fn notify_touch_down(
        &self,
        _s: ObjectPath<'_>,
        _o: HashMap<String, OwnedValue>,
        stream: u32,
        slot: u32,
        x: f64,
        y: f64,
    ) {
        self.0
            .note(format!("TouchDown stream={stream} slot={slot} {x} {y}"));
    }

    fn notify_touch_motion(
        &self,
        _s: ObjectPath<'_>,
        _o: HashMap<String, OwnedValue>,
        stream: u32,
        slot: u32,
        x: f64,
        y: f64,
    ) {
        self.0
            .note(format!("TouchMotion stream={stream} slot={slot} {x} {y}"));
    }

    fn notify_touch_up(&self, _s: ObjectPath<'_>, _o: HashMap<String, OwnedValue>, slot: u32) {
        self.0.note(format!("TouchUp slot={slot}"));
    }
}

fn request_path(
    hdr: &Header<'_>,
    options: &HashMap<String, OwnedValue>,
) -> zbus::zvariant::OwnedObjectPath {
    ObjectPath::try_from(object(hdr, "request", string(options, "handle_token")))
        .unwrap()
        .into()
}

struct FakeCast(Shared);

#[zbus::interface(name = "org.freedesktop.portal.ScreenCast")]
impl FakeCast {
    #[zbus(property)]
    fn version(&self) -> u32 {
        5
    }

    #[zbus(property)]
    fn available_source_types(&self) -> u32 {
        7
    }

    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        7
    }

    async fn select_sources(
        &self,
        _session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] hdr: Header<'_>,
    ) -> zbus::zvariant::OwnedObjectPath {
        let types = options
            .get("types")
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        self.0.note(format!("SelectSources types={types}"));
        self.0.respond(&hdr, &options, HashMap::new());
        request_path(&hdr, &options)
    }

    fn open_pipe_wire_remote(
        &self,
        _session: ObjectPath<'_>,
        _o: HashMap<String, OwnedValue>,
    ) -> OwnedFd {
        self.0.note("OpenPipeWireRemote".into());
        let f = std::fs::File::open("/dev/null").unwrap();
        OwnedFd::from(std::os::fd::OwnedFd::from(f))
    }
}

struct FakeSession(Shared);

#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl FakeSession {
    fn close(&self) {
        self.0.note("Session.Close".into());
    }
}

/// Starts the fake on the session bus with the portal's well-known name. `devices` is what it grants (1 keyboard,
/// 2 pointer, 4 touch screen).
pub async fn serve(devices: u32) -> (Connection, Log) {
    let log: Log = Arc::default();
    let shared = Shared {
        log: log.clone(),
        conn: Arc::default(),
        devices,
    };
    let conn = zbus::connection::Builder::session()
        .unwrap()
        .serve_at(
            "/org/freedesktop/portal/desktop",
            FakeRemote(shared.clone()),
        )
        .unwrap()
        .serve_at("/org/freedesktop/portal/desktop", FakeCast(shared.clone()))
        .unwrap()
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .build()
        .await
        .unwrap();
    *shared.conn.lock().unwrap() = Some(conn.clone());
    (conn, log)
}
