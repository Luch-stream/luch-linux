//! Puts the events on this computer: through the RemoteDesktop portal where it has the device, through uinput
//! where it has not (a gamepad, a stylus with pressure, a compositor without the portal), and a touch turned into
//! the pointer where neither has a touch screen.

use std::{collections::BTreeMap, sync::Arc};

use ashpd::desktop::remote_desktop::{Axis, DeviceType, KeyState, NotifyPointerAxisOptions};

use super::map::Geometry;
use super::portal::Remote;
use super::state::{Event, Pen, BTN_LEFT};
use super::uinput::{self, Device, Ev, EV_KEY, EV_REL};

/// The touch slot a stylus takes where it can only be a touch.
const PEN_SLOT: u32 = 15;

pub struct Injector {
    portal: Option<Arc<Remote>>,
    uinput: bool,
    geo: Geometry,
    kbd_mouse: Option<Device>,
    touch: Option<Device>,
    pen: Option<Device>,
    pads: BTreeMap<u8, Device>,
    /// uinput touch slots down now, with their tracking ids.
    slots: BTreeMap<u32, i32>,
    next_tracking: i32,
    last_pen: Option<Pen>,
    /// Without a touch screen device the first contact drives the pointer: its slot.
    pointer_finger: Option<u32>,
    /// Whether the stylus is down as a touch (no pen device).
    pen_as_touch: bool,
}

impl Injector {
    pub fn new(portal: Option<Arc<Remote>>, uinput: bool, geo: Geometry) -> Self {
        Injector {
            portal,
            uinput,
            geo,
            kbd_mouse: None,
            touch: None,
            pen: None,
            pads: BTreeMap::new(),
            slots: BTreeMap::new(),
            next_tracking: 0,
            last_pen: None,
            pointer_finger: None,
            pen_as_touch: false,
        }
    }

    fn has(&self, d: DeviceType) -> bool {
        self.portal.as_ref().is_some_and(|p| p.devices.contains(d))
    }

    /// What the grant's `accept` lists (PROTOCOL.md 4h).
    pub fn accepts(&self) -> Vec<&'static str> {
        let pointer = self.has(DeviceType::POINTER) || self.uinput;
        let mut v = vec![];
        // A touch becomes the pointer where there is no touch screen; only an absolute pointer can follow it.
        if self.has(DeviceType::TOUCHSCREEN) || self.has(DeviceType::POINTER) || self.uinput {
            v.push("touch");
        }
        if pointer {
            v.push("pointer");
        }
        if self.has(DeviceType::KEYBOARD) || self.uinput {
            v.push("keyboard");
        }
        if self.uinput {
            v.extend(["gamepad", "pen"]);
        }
        v
    }

    pub fn geometry(&self) -> &Geometry {
        &self.geo
    }

    fn device(slot: &mut Option<Device>, spec: fn() -> uinput::Spec) -> Option<&Device> {
        if slot.is_none() {
            match Device::create(&spec()) {
                Ok(d) => *slot = Some(d),
                Err(e) => {
                    eprintln!("[luchd] input: uinput {}: {e}", spec().name);
                    return None;
                }
            }
        }
        slot.as_ref()
    }

    fn emit(slot: &mut Option<Device>, spec: fn() -> uinput::Spec, ev: &[Ev]) {
        if let Some(d) = Self::device(slot, spec) {
            if let Err(e) = d.emit(ev) {
                eprintln!("[luchd] input: uinput write: {e}");
            }
        }
    }

    pub async fn inject(&mut self, ev: Event) {
        if let Err(e) = self.route(ev).await {
            eprintln!("[luchd] input: {e}");
        }
    }

    async fn route(&mut self, ev: Event) -> ashpd::Result<()> {
        match ev {
            Event::TouchDown { slot, x, y } => self.touch(slot, Some((x, y)), true).await?,
            Event::TouchMotion { slot, x, y } => self.touch(slot, Some((x, y)), false).await?,
            Event::TouchUp { slot } => self.touch(slot, None, false).await?,
            Event::Pen(p) => self.pen(Some(p)).await?,
            Event::PenOut => self.pen(None).await?,
            Event::PointerAbs { x, y } => self.pointer_to(x, y).await?,
            Event::PointerRel { dx, dy } => {
                if let Some(p) = self
                    .portal
                    .clone()
                    .filter(|_| self.has(DeviceType::POINTER))
                {
                    p.rd.notify_pointer_motion(&p.session, dx, dy, Default::default())
                        .await?;
                } else if self.uinput {
                    Self::emit(
                        &mut self.kbd_mouse,
                        uinput::keyboard_mouse,
                        &[
                            (EV_REL, uinput::REL_X, dx.round() as i32),
                            (EV_REL, uinput::REL_Y, dy.round() as i32),
                        ],
                    );
                }
            }
            Event::Button { code, down } => self.button(code, down).await?,
            Event::Scroll { dx, dy, finish } => {
                if let Some(p) = self
                    .portal
                    .clone()
                    .filter(|_| self.has(DeviceType::POINTER))
                {
                    p.rd.notify_pointer_axis(
                        &p.session,
                        dx,
                        dy,
                        NotifyPointerAxisOptions::default().set_finish(finish),
                    )
                    .await?;
                } else if self.uinput {
                    // 120 per notch of PX_PER_NOTCH pixels; the wheel's own axis counts up for "up".
                    let hi = |px: f64| (px / super::state::PX_PER_NOTCH * 120.0).round() as i32;
                    Self::emit(
                        &mut self.kbd_mouse,
                        uinput::keyboard_mouse,
                        &[
                            (EV_REL, uinput::REL_WHEEL_HI_RES, -hi(dy)),
                            (EV_REL, uinput::REL_HWHEEL_HI_RES, hi(dx)),
                        ],
                    );
                }
            }
            Event::Notches {
                vertical,
                horizontal,
            } => {
                if let Some(p) = self
                    .portal
                    .clone()
                    .filter(|_| self.has(DeviceType::POINTER))
                {
                    for (axis, steps) in
                        [(Axis::Vertical, vertical), (Axis::Horizontal, horizontal)]
                    {
                        if steps != 0 {
                            p.rd.notify_pointer_axis_discrete(
                                &p.session,
                                axis,
                                steps,
                                Default::default(),
                            )
                            .await?;
                        }
                    }
                } else if self.uinput {
                    Self::emit(
                        &mut self.kbd_mouse,
                        uinput::keyboard_mouse,
                        &[
                            (EV_REL, uinput::REL_WHEEL, -vertical),
                            (EV_REL, uinput::REL_WHEEL_HI_RES, -vertical * 120),
                            (EV_REL, uinput::REL_HWHEEL, horizontal),
                            (EV_REL, uinput::REL_HWHEEL_HI_RES, horizontal * 120),
                        ],
                    );
                }
            }
            Event::Key { code, down } => {
                if let Some(p) = self
                    .portal
                    .clone()
                    .filter(|_| self.has(DeviceType::KEYBOARD))
                {
                    p.rd.notify_keyboard_keycode(
                        &p.session,
                        code as i32,
                        state(down),
                        Default::default(),
                    )
                    .await?;
                } else if self.uinput {
                    Self::emit(
                        &mut self.kbd_mouse,
                        uinput::keyboard_mouse,
                        &[(EV_KEY, code, down as i32)],
                    );
                }
            }
            Event::Pad(pad) if self.uinput => {
                let mut slot = self.pads.remove(&pad.index);
                Self::emit(&mut slot, uinput::gamepad, &uinput::pad_events(&pad));
                if let Some(d) = slot {
                    self.pads.insert(pad.index, d);
                }
            }
            Event::PadGone(i) => {
                self.pads.remove(&i); // dropping the device unplugs it
            }
            Event::Pad(_) => {}
        }
        Ok(())
    }

    async fn pointer_to(&mut self, x: f64, y: f64) -> ashpd::Result<()> {
        if let Some(p) = self
            .portal
            .clone()
            .filter(|_| self.has(DeviceType::POINTER))
        {
            let (sx, sy) = self.geo.stream_point(x, y);
            p.rd.notify_pointer_motion_absolute(&p.session, p.node, sx, sy, Default::default())
                .await?;
        }
        // uinput has no absolute pointer here: a mouse on the receiver is captured and moves relatively.
        Ok(())
    }

    async fn button(&mut self, code: u16, down: bool) -> ashpd::Result<()> {
        if let Some(p) = self
            .portal
            .clone()
            .filter(|_| self.has(DeviceType::POINTER))
        {
            p.rd.notify_pointer_button(&p.session, code as i32, state(down), Default::default())
                .await?;
        } else if self.uinput {
            Self::emit(
                &mut self.kbd_mouse,
                uinput::keyboard_mouse,
                &[(EV_KEY, code, down as i32)],
            );
        }
        Ok(())
    }

    async fn touch(&mut self, slot: u32, at: Option<(f64, f64)>, down: bool) -> ashpd::Result<()> {
        if let Some(p) = self
            .portal
            .clone()
            .filter(|_| self.has(DeviceType::TOUCHSCREEN))
        {
            match at {
                Some((x, y)) => {
                    let (sx, sy) = self.geo.stream_point(x, y);
                    if down {
                        p.rd.notify_touch_down(
                            &p.session,
                            p.node,
                            slot,
                            sx,
                            sy,
                            Default::default(),
                        )
                        .await?;
                    } else {
                        p.rd.notify_touch_motion(
                            &p.session,
                            p.node,
                            slot,
                            sx,
                            sy,
                            Default::default(),
                        )
                        .await?;
                    }
                }
                None => {
                    p.rd.notify_touch_up(&p.session, slot, Default::default())
                        .await?
                }
            }
        } else if self.uinput && !self.has(DeviceType::POINTER) {
            let first = self.slots.is_empty();
            let tracking = match (at, down) {
                (Some(_), true) => {
                    self.next_tracking = (self.next_tracking + 1) & 0xffff;
                    self.slots.insert(slot, self.next_tracking);
                    self.next_tracking
                }
                (None, _) => {
                    self.slots.remove(&slot);
                    -1
                }
                _ => -1,
            };
            let fraction = at.map(|(x, y)| self.geo.layout_fraction(x, y));
            let ev = uinput::touch_events(
                slot % uinput::SLOTS as u32,
                tracking,
                fraction,
                down && first,
                at.is_none() && self.slots.is_empty(),
            );
            Self::emit(&mut self.touch, uinput::touchscreen, &ev);
        } else {
            // No touch screen device: the first finger is the pointer, the others are ignored.
            if self.pointer_finger.is_none() && down {
                self.pointer_finger = Some(slot);
            }
            if self.pointer_finger != Some(slot) {
                return Ok(());
            }
            match at {
                Some((x, y)) => {
                    self.pointer_to(x, y).await?;
                    if down {
                        self.button(BTN_LEFT, true).await?;
                    }
                }
                None => {
                    self.pointer_finger = None;
                    self.button(BTN_LEFT, false).await?;
                }
            }
        }
        Ok(())
    }

    async fn pen(&mut self, p: Option<Pen>) -> ashpd::Result<()> {
        if self.uinput {
            let at = p.map(|p| self.geo.layout_fraction(p.x, p.y));
            let ev = uinput::pen_events(self.last_pen.as_ref(), p.as_ref().zip(at));
            self.last_pen = p;
            Self::emit(&mut self.pen, uinput::pen, &ev);
            return Ok(());
        }
        // No pen device: touching is a touch, hovering moves the pointer.
        match p {
            Some(p) if p.touching => {
                let down = !self.pen_as_touch;
                self.pen_as_touch = true;
                self.touch(PEN_SLOT, Some((p.x, p.y)), down).await?;
            }
            other => {
                if std::mem::take(&mut self.pen_as_touch) {
                    self.touch(PEN_SLOT, None, false).await?;
                }
                if let Some(p) = other {
                    self.pointer_to(p.x, p.y).await?;
                }
            }
        }
        Ok(())
    }
}

fn state(down: bool) -> KeyState {
    if down {
        KeyState::Pressed
    } else {
        KeyState::Released
    }
}
