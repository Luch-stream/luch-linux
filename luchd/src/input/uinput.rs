//! Virtual input devices through /dev/uinput, for what the RemoteDesktop portal has no device for (a gamepad, a
//! stylus with pressure) and for compositors without that portal (niri, wlroots). Needs write access to
//! /dev/uinput: the `input` group or the udev rule Steam's `steam-devices` installs. Never root.

use std::{
    fs::OpenOptions,
    io::{self, Write},
    os::fd::{AsRawFd, OwnedFd},
};

use super::state::Pen;
use super::wire::Pad;

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_REL: u16 = 2;
pub const EV_ABS: u16 = 3;

pub const REL_X: u16 = 0;
pub const REL_Y: u16 = 1;
pub const REL_HWHEEL: u16 = 6;
pub const REL_WHEEL: u16 = 8;
pub const REL_WHEEL_HI_RES: u16 = 11;
pub const REL_HWHEEL_HI_RES: u16 = 12;

pub const ABS_X: u16 = 0;
pub const ABS_Y: u16 = 1;
pub const ABS_Z: u16 = 2;
pub const ABS_RX: u16 = 3;
pub const ABS_RY: u16 = 4;
pub const ABS_RZ: u16 = 5;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;
pub const ABS_PRESSURE: u16 = 0x18;
pub const ABS_MT_SLOT: u16 = 0x2f;
pub const ABS_MT_POSITION_X: u16 = 0x35;
pub const ABS_MT_POSITION_Y: u16 = 0x36;
pub const ABS_MT_TRACKING_ID: u16 = 0x39;

pub const BTN_TOOL_PEN: u16 = 0x140;
pub const BTN_TOOL_RUBBER: u16 = 0x141;
pub const BTN_TOUCH: u16 = 0x14a;
pub const BTN_STYLUS: u16 = 0x14b;
const INPUT_PROP_DIRECT: u16 = 1;

/// Absolute axes of the pen and the touch screen span 0..=ABS_MAX over the whole desktop.
pub const ABS_MAX: i32 = 32767;
const PRESSURE_MAX: i32 = 4095;
pub const SLOTS: i32 = 16;

/// The Xbox 360 pad's buttons in xpad's codes, in the SDL order of the gamepad record: games and Steam know
/// this device (045e:028e) without a mapping.
const PAD_BUTTONS: [u16; 11] = [
    0x130, // A        BTN_SOUTH
    0x131, // B        BTN_EAST
    0x133, // X        BTN_NORTH (xpad's BTN_X)
    0x134, // Y        BTN_WEST (xpad's BTN_Y)
    0x13a, // back     BTN_SELECT
    0x13c, // guide    BTN_MODE
    0x13b, // start    BTN_START
    0x13d, // l. stick BTN_THUMBL
    0x13e, // r. stick BTN_THUMBR
    0x136, // LB       BTN_TL
    0x137, // RB       BTN_TR
];

pub type Ev = (u16, u16, i32);

pub struct Abs {
    pub code: u16,
    pub min: i32,
    pub max: i32,
    pub res: i32,
}

pub struct Spec {
    pub name: &'static str,
    pub vendor: u16,
    pub product: u16,
    pub keys: Vec<u16>,
    pub rels: Vec<u16>,
    pub abs: Vec<Abs>,
    pub direct: bool,
}

fn abs(code: u16, min: i32, max: i32, res: i32) -> Abs {
    Abs {
        code,
        min,
        max,
        res,
    }
}

/// A keyboard and a relative mouse with a high-resolution wheel.
pub fn keyboard_mouse() -> Spec {
    Spec {
        name: "Luch keyboard and mouse",
        vendor: 0x1d6b, // Linux Foundation: a virtual device, like uinput's own examples
        product: 0x4c01,
        keys: (1..=255).chain(0x110..=0x117).collect(),
        rels: vec![
            REL_X,
            REL_Y,
            REL_WHEEL,
            REL_HWHEEL,
            REL_WHEEL_HI_RES,
            REL_HWHEEL_HI_RES,
        ],
        abs: vec![],
        direct: false,
    }
}

pub fn touchscreen() -> Spec {
    Spec {
        name: "Luch touch screen",
        vendor: 0x1d6b,
        product: 0x4c02,
        keys: vec![BTN_TOUCH],
        rels: vec![],
        abs: vec![
            abs(ABS_X, 0, ABS_MAX, 0),
            abs(ABS_Y, 0, ABS_MAX, 0),
            abs(ABS_MT_SLOT, 0, SLOTS - 1, 0),
            abs(ABS_MT_TRACKING_ID, 0, 65535, 0),
            abs(ABS_MT_POSITION_X, 0, ABS_MAX, 0),
            abs(ABS_MT_POSITION_Y, 0, ABS_MAX, 0),
        ],
        direct: true,
    }
}

/// A screen tablet; libinput wants a resolution on its axes.
pub fn pen() -> Spec {
    Spec {
        name: "Luch stylus",
        vendor: 0x1d6b,
        product: 0x4c03,
        keys: vec![BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH, BTN_STYLUS],
        rels: vec![],
        abs: vec![
            abs(ABS_X, 0, ABS_MAX, 100),
            abs(ABS_Y, 0, ABS_MAX, 100),
            abs(ABS_PRESSURE, 0, PRESSURE_MAX, 0),
        ],
        direct: true,
    }
}

pub fn gamepad() -> Spec {
    Spec {
        name: "Luch gamepad (Xbox 360 layout)",
        vendor: 0x045e,
        product: 0x028e,
        keys: PAD_BUTTONS.to_vec(),
        rels: vec![],
        abs: vec![
            abs(ABS_X, -32768, 32767, 0),
            abs(ABS_Y, -32768, 32767, 0),
            abs(ABS_RX, -32768, 32767, 0),
            abs(ABS_RY, -32768, 32767, 0),
            abs(ABS_Z, 0, 255, 0),
            abs(ABS_RZ, 0, 255, 0),
            abs(ABS_HAT0X, -1, 1, 0),
            abs(ABS_HAT0Y, -1, 1, 0),
        ],
        direct: false,
    }
}

/// The whole state of a pad; the kernel passes on only what changed.
pub fn pad_events(p: &Pad) -> Vec<Ev> {
    let bit = |i: u32| p.buttons & 1 << i != 0;
    let mut v: Vec<Ev> = PAD_BUTTONS
        .iter()
        .enumerate()
        .map(|(i, c)| (EV_KEY, *c, bit(i as u32) as i32))
        .collect();
    let hat = |neg: u32, pos: u32| bit(pos) as i32 - bit(neg) as i32;
    v.extend([
        (EV_ABS, ABS_X, p.left.0 as i32),
        (EV_ABS, ABS_Y, p.left.1 as i32),
        (EV_ABS, ABS_RX, p.right.0 as i32),
        (EV_ABS, ABS_RY, p.right.1 as i32),
        (EV_ABS, ABS_Z, (p.triggers.0 >> 8) as i32),
        (EV_ABS, ABS_RZ, (p.triggers.1 >> 8) as i32),
        (EV_ABS, ABS_HAT0X, hat(13, 14)),
        (EV_ABS, ABS_HAT0Y, hat(11, 12)),
    ]);
    v
}

/// A pen coming, moving, pressing or leaving; `at` is its point as a fraction of the desktop.
pub fn pen_events(old: Option<&Pen>, new: Option<(&Pen, (f64, f64))>) -> Vec<Ev> {
    let tool = |p: &Pen| {
        if p.eraser {
            BTN_TOOL_RUBBER
        } else {
            BTN_TOOL_PEN
        }
    };
    let mut v = vec![];
    match (old, new) {
        (Some(o), None) => {
            if o.touching {
                v.extend([(EV_KEY, BTN_TOUCH, 0), (EV_ABS, ABS_PRESSURE, 0)]);
            }
            v.push((EV_KEY, tool(o), 0));
        }
        (old, Some((p, (fx, fy)))) => {
            if let Some(o) = old.filter(|o| tool(o) != tool(p)) {
                v.extend(pen_events(Some(o), None));
            }
            if old.is_none_or(|o| tool(o) != tool(p)) {
                v.push((EV_KEY, tool(p), 1));
            }
            v.extend([
                (EV_ABS, ABS_X, scale(fx)),
                (EV_ABS, ABS_Y, scale(fy)),
                (
                    EV_ABS,
                    ABS_PRESSURE,
                    (p.pressure.clamp(0.0, 1.0) * PRESSURE_MAX as f64).round() as i32,
                ),
                (EV_KEY, BTN_TOUCH, p.touching as i32),
                (EV_KEY, BTN_STYLUS, p.button as i32),
            ]);
        }
        (None, None) => {}
    }
    v
}

/// One contact of the touch screen. `at` None lifts it; `first`/`last` say it is the only one, for BTN_TOUCH.
pub fn touch_events(
    slot: u32,
    tracking: i32,
    at: Option<(f64, f64)>,
    first: bool,
    last: bool,
) -> Vec<Ev> {
    let mut v = vec![(EV_ABS, ABS_MT_SLOT, slot as i32)];
    match at {
        Some((fx, fy)) => {
            if tracking >= 0 {
                v.push((EV_ABS, ABS_MT_TRACKING_ID, tracking));
            }
            v.extend([
                (EV_ABS, ABS_MT_POSITION_X, scale(fx)),
                (EV_ABS, ABS_MT_POSITION_Y, scale(fy)),
            ]);
            v.extend([(EV_ABS, ABS_X, scale(fx)), (EV_ABS, ABS_Y, scale(fy))]);
            if first {
                v.push((EV_KEY, BTN_TOUCH, 1));
            }
        }
        None => {
            v.push((EV_ABS, ABS_MT_TRACKING_ID, -1));
            if last {
                v.push((EV_KEY, BTN_TOUCH, 0));
            }
        }
    }
    v
}

fn scale(f: f64) -> i32 {
    (f.clamp(0.0, 1.0) * ABS_MAX as f64).round() as i32
}

// ---- the device itself ----------------------------------------------------------------------------

const fn iow(nr: u64, size: u64) -> u64 {
    (1 << 30) | (size << 16) | ((b'U' as u64) << 8) | nr
}
const UI_SET_EVBIT: u64 = iow(100, 4);
const UI_SET_KEYBIT: u64 = iow(101, 4);
const UI_SET_RELBIT: u64 = iow(102, 4);
const UI_SET_ABSBIT: u64 = iow(103, 4);
const UI_SET_PROPBIT: u64 = iow(110, 4);
const UI_DEV_SETUP: u64 = iow(3, std::mem::size_of::<UinputSetup>() as u64);
const UI_ABS_SETUP: u64 = iow(4, std::mem::size_of::<UinputAbsSetup>() as u64);
const UI_DEV_CREATE: u64 = (b'U' as u64) << 8 | 1;
const UI_DEV_DESTROY: u64 = (b'U' as u64) << 8 | 2;

#[repr(C)]
struct UinputSetup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct InputEvent {
    time: libc::timeval,
    kind: u16,
    code: u16,
    value: i32,
}

/// Whether this user may make devices at all.
pub fn allowed() -> bool {
    std::env::var("LUCH_INPUT").as_deref() != Ok("portal")
        && OpenOptions::new().write(true).open("/dev/uinput").is_ok()
}

pub struct Device {
    fd: OwnedFd,
}

fn ioctl<T>(fd: &OwnedFd, req: u64, arg: T) -> io::Result<()> {
    // SAFETY: the requests above take either an int or a pointer to the matching #[repr(C)] struct.
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), req as _, arg) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl Device {
    pub fn create(spec: &Spec) -> io::Result<Self> {
        let fd: OwnedFd = OpenOptions::new().write(true).open("/dev/uinput")?.into();
        if !spec.keys.is_empty() {
            ioctl(&fd, UI_SET_EVBIT, EV_KEY as libc::c_int)?;
            for k in &spec.keys {
                ioctl(&fd, UI_SET_KEYBIT, *k as libc::c_int)?;
            }
        }
        if !spec.rels.is_empty() {
            ioctl(&fd, UI_SET_EVBIT, EV_REL as libc::c_int)?;
            for r in &spec.rels {
                ioctl(&fd, UI_SET_RELBIT, *r as libc::c_int)?;
            }
        }
        if !spec.abs.is_empty() {
            ioctl(&fd, UI_SET_EVBIT, EV_ABS as libc::c_int)?;
            for a in &spec.abs {
                ioctl(&fd, UI_SET_ABSBIT, a.code as libc::c_int)?;
                let setup = UinputAbsSetup {
                    code: a.code,
                    value: 0,
                    minimum: a.min,
                    maximum: a.max,
                    fuzz: 0,
                    flat: 0,
                    resolution: a.res,
                };
                ioctl(&fd, UI_ABS_SETUP, &setup as *const UinputAbsSetup)?;
            }
        }
        if spec.direct {
            ioctl(&fd, UI_SET_PROPBIT, INPUT_PROP_DIRECT as libc::c_int)?;
        }
        let mut name = [0u8; 80];
        name[..spec.name.len()].copy_from_slice(spec.name.as_bytes());
        let setup = UinputSetup {
            bustype: 0x03, // BUS_USB
            vendor: spec.vendor,
            product: spec.product,
            version: 1,
            name,
            ff_effects_max: 0,
        };
        ioctl(&fd, UI_DEV_SETUP, &setup as *const UinputSetup)?;
        ioctl(&fd, UI_DEV_CREATE, 0 as libc::c_int)?;
        Ok(Device { fd })
    }

    /// Writes the events and the SYN_REPORT that makes them one frame.
    pub fn emit(&self, evs: &[Ev]) -> io::Result<()> {
        if evs.is_empty() {
            return Ok(());
        }
        let mut buf = Vec::with_capacity((evs.len() + 1) * std::mem::size_of::<InputEvent>());
        for &(kind, code, value) in evs.iter().chain([&(EV_SYN, 0, 0)]) {
            let e = InputEvent {
                time: libc::timeval {
                    tv_sec: 0,
                    tv_usec: 0,
                },
                kind,
                code,
                value,
            };
            // SAFETY: InputEvent is plain old data, read as its own bytes.
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    &e as *const InputEvent as *const u8,
                    std::mem::size_of::<InputEvent>(),
                )
            };
            buf.extend_from_slice(bytes);
        }
        let mut f = std::fs::File::from(self.fd.try_clone()?);
        f.write_all(&buf)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = ioctl(&self.fd, UI_DEV_DESTROY, 0 as libc::c_int);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_linux_uinput_h() {
        assert_eq!(UI_SET_EVBIT, 0x4004_5564);
        assert_eq!(UI_SET_KEYBIT, 0x4004_5565);
        assert_eq!(UI_SET_ABSBIT, 0x4004_5567);
        assert_eq!(UI_SET_PROPBIT, 0x4004_556e);
        assert_eq!(UI_DEV_SETUP, 0x405c_5503);
        assert_eq!(UI_ABS_SETUP, 0x401c_5504);
        assert_eq!(UI_DEV_CREATE, 0x5501);
        assert_eq!(std::mem::size_of::<InputEvent>(), 24);
    }

    #[test]
    fn gamepad_state_in_xpad_codes() {
        let p = Pad {
            index: 0,
            connected: true,
            buttons: 1 | 1 << 2 | 1 << 11 | 1 << 14, // A, X, up, right
            left: (-32768, 32767),
            right: (5, -5),
            triggers: (65535, 256),
        };
        let ev = pad_events(&p);
        assert!(
            ev.contains(&(EV_KEY, 0x130, 1))
                && ev.contains(&(EV_KEY, 0x133, 1))
                && ev.contains(&(EV_KEY, 0x131, 0))
        );
        assert!(ev.contains(&(EV_ABS, ABS_X, -32768)) && ev.contains(&(EV_ABS, ABS_Y, 32767)));
        assert!(ev.contains(&(EV_ABS, ABS_Z, 255)) && ev.contains(&(EV_ABS, ABS_RZ, 1)));
        assert!(ev.contains(&(EV_ABS, ABS_HAT0X, 1)) && ev.contains(&(EV_ABS, ABS_HAT0Y, -1)));
    }

    #[test]
    fn pen_comes_presses_switches_to_eraser_and_leaves() {
        let pen = Pen {
            x: 0.0,
            y: 0.0,
            pressure: 0.0,
            touching: false,
            eraser: false,
            button: false,
        };
        let ev = pen_events(None, Some((&pen, (0.5, 1.0))));
        assert_eq!(ev[0], (EV_KEY, BTN_TOOL_PEN, 1));
        assert!(ev.contains(&(EV_ABS, ABS_X, 16384)) && ev.contains(&(EV_ABS, ABS_Y, ABS_MAX)));
        assert!(ev.contains(&(EV_KEY, BTN_TOUCH, 0)));
        let down = Pen {
            pressure: 0.5,
            touching: true,
            ..pen
        };
        let ev = pen_events(Some(&pen), Some((&down, (0.5, 1.0))));
        assert!(
            !ev.iter().any(|e| e.1 == BTN_TOOL_PEN),
            "the tool is already in"
        );
        assert!(ev.contains(&(EV_ABS, ABS_PRESSURE, 2048)) && ev.contains(&(EV_KEY, BTN_TOUCH, 1)));
        let eraser = Pen {
            eraser: true,
            ..down
        };
        let ev = pen_events(Some(&down), Some((&eraser, (0.0, 0.0))));
        assert_eq!(
            &ev[..4],
            &[
                (EV_KEY, BTN_TOUCH, 0),
                (EV_ABS, ABS_PRESSURE, 0),
                (EV_KEY, BTN_TOOL_PEN, 0),
                (EV_KEY, BTN_TOOL_RUBBER, 1)
            ]
        );
        assert_eq!(
            pen_events(Some(&pen), None),
            vec![(EV_KEY, BTN_TOOL_PEN, 0)]
        );
    }

    #[test]
    fn touch_contacts_use_type_b_slots() {
        assert_eq!(
            touch_events(1, 7, Some((1.0, 0.0)), true, false),
            vec![
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_TRACKING_ID, 7),
                (EV_ABS, ABS_MT_POSITION_X, ABS_MAX),
                (EV_ABS, ABS_MT_POSITION_Y, 0),
                (EV_ABS, ABS_X, ABS_MAX),
                (EV_ABS, ABS_Y, 0),
                (EV_KEY, BTN_TOUCH, 1),
            ]
        );
        assert_eq!(
            touch_events(1, -1, None, false, true),
            vec![
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_TRACKING_ID, -1),
                (EV_KEY, BTN_TOUCH, 0)
            ]
        );
    }

    /// Makes a real gamepad and reads its events back from the kernel. Needs /dev/uinput, so it runs only in a
    /// privileged container: `docker run --privileged … cargo test -p luchd uinput_device -- --ignored`.
    #[test]
    #[ignore]
    fn uinput_device_round_trip() {
        use std::io::Read;
        // The kernel takes every device the injector makes.
        for spec in [keyboard_mouse(), touchscreen(), pen()] {
            let d = Device::create(&spec).unwrap_or_else(|e| panic!("{}: {e}", spec.name));
            d.emit(&[(EV_ABS, ABS_X, 1)]).ok();
        }
        let dev = Device::create(&gamepad()).expect("create");
        // Find the event node the kernel made for it and open it (containers have no /dev/input).
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut node = None;
        for e in std::fs::read_dir("/sys/class/input").unwrap().flatten() {
            let name = std::fs::read_to_string(e.path().join("device/name")).unwrap_or_default();
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("event") && name.trim() == gamepad().name {
                let dev = std::fs::read_to_string(e.path().join("dev")).unwrap();
                let (ma, mi) = dev.trim().split_once(':').unwrap();
                node = Some((n, ma.parse::<u32>().unwrap(), mi.parse::<u32>().unwrap()));
            }
        }
        let (n, ma, mi) = node.expect("event node of the gamepad");
        let path = format!("/tmp/{n}");
        let c = std::ffi::CString::new(path.clone()).unwrap();
        // SAFETY: plain mknod of a character device node in /tmp.
        let _ = unsafe { libc::mknod(c.as_ptr(), libc::S_IFCHR | 0o600, libc::makedev(ma, mi)) };
        let mut f = std::fs::File::open(&path).unwrap();
        dev.emit(&[(EV_KEY, 0x130, 1), (EV_ABS, ABS_X, -1234)])
            .unwrap();
        let mut buf = [0u8; 24 * 3];
        f.read_exact(&mut buf).unwrap();
        let ev = |i: usize| {
            let b = &buf[i * 24 + 16..i * 24 + 24];
            (
                u16::from_ne_bytes([b[0], b[1]]),
                u16::from_ne_bytes([b[2], b[3]]),
                i32::from_ne_bytes([b[4], b[5], b[6], b[7]]),
            )
        };
        assert_eq!(
            (ev(0), ev(1), ev(2)),
            ((EV_KEY, 0x130, 1), (EV_ABS, ABS_X, -1234), (EV_SYN, 0, 0))
        );
        let _ = std::fs::remove_file(path);
    }
}
