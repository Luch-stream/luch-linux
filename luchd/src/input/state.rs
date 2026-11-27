//! Turns the receiver's successive states into input events: what went down, what moved, what was let go.
//! Every injector (the portal, uinput) consumes the same events.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use super::wire::{self, Contact, Packet, Pad, Record, Tool};

/// No datagram for this long while something is held: everything is let go (PROTOCOL.md 4i).
pub const WATCHDOG: Duration = Duration::from_millis(500);
/// Logical pixels one wheel notch scrolls on a smooth (touchpad) wheel: libinput's figure for a notch.
pub const PX_PER_NOTCH: f64 = 15.0;

pub const BTN_LEFT: u16 = 0x110;
/// Pointer buttons in the order of the record's bits.
const BUTTONS: [u16; 5] = [BTN_LEFT, 0x111, 0x112, 0x116, 0x115]; // left, right, middle, back (side), forward (extra)

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pen {
    pub x: f64,
    pub y: f64,
    /// 0..1; a touch with unknown pressure counts as half.
    pub pressure: f64,
    pub touching: bool,
    pub eraser: bool,
    pub button: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Positions are normalized to the picture, 0..1.
    TouchDown {
        slot: u32,
        x: f64,
        y: f64,
    },
    TouchMotion {
        slot: u32,
        x: f64,
        y: f64,
    },
    TouchUp {
        slot: u32,
    },
    /// The stylus is near the screen or on it.
    Pen(Pen),
    /// The stylus went away.
    PenOut,
    PointerAbs {
        x: f64,
        y: f64,
    },
    /// Logical pixels.
    PointerRel {
        dx: f64,
        dy: f64,
    },
    Button {
        code: u16,
        down: bool,
    },
    /// Touchpad scrolling in logical pixels; `finish` ends a run (kinetic scrolling starts there).
    Scroll {
        dx: f64,
        dy: f64,
        finish: bool,
    },
    /// Mouse wheel notches, positive down / right.
    Notches {
        vertical: i32,
        horizontal: i32,
    },
    Key {
        code: u16,
        down: bool,
    },
    Pad(Pad),
    PadGone(u8),
}

pub fn norm(v: u16) -> f64 {
    v as f64 / 65535.0
}

#[derive(Default)]
pub struct Tracker {
    pub token: u64,
    last_seq: Option<u32>,
    last_rx: Option<Instant>,
    fingers: BTreeMap<u8, (u16, u16)>,
    pen: Option<Pen>,
    pointer: Option<wire::Pointer>,
    wheel_rest: (i32, i32),
    scrolling: bool,
    keys_applied: Option<u16>,
    held: BTreeSet<u16>,
    pads: BTreeMap<u8, Pad>,
}

impl Tracker {
    pub fn new(token: u64) -> Self {
        Tracker {
            token,
            ..Default::default()
        }
    }

    /// Whether anything is down: a contact, a button, a key, a gamepad off centre.
    pub fn busy(&self) -> bool {
        !self.fingers.is_empty()
            || self.pen.is_some_and(|p| p.touching)
            || self.pointer.is_some_and(|p| p.buttons != 0)
            || !self.held.is_empty()
            || self.pads.values().any(|p| *p != neutral(p.index))
    }

    /// The events a datagram brings, or none when it is late, repeated or not ours.
    pub fn apply(&mut self, p: &Packet, now: Instant) -> Vec<Event> {
        if p.token != self.token || self.last_seq.is_some_and(|s| !wire::newer(p.seq, s)) {
            return vec![];
        }
        self.last_seq = Some(p.seq);
        self.last_rx = Some(now);
        let mut out = vec![];
        for r in &p.records {
            match r {
                Record::Touch(cs) => self.touch(cs, &mut out),
                Record::Pointer(ptr) => self.pointer(ptr, &mut out),
                Record::Keys(k) => self.keys(k, &mut out),
                Record::Gamepad(g) => self.pad(g, &mut out),
            }
        }
        out
    }

    /// Lets everything go when the receiver has fallen silent with something held.
    pub fn watchdog(&mut self, now: Instant) -> Vec<Event> {
        match self.last_rx {
            Some(t) if now.duration_since(t) >= WATCHDOG && self.busy() => self.release_all(),
            _ => vec![],
        }
    }

    /// Everything up, the gamepads centred: the end of a session or a silent receiver.
    pub fn release_all(&mut self) -> Vec<Event> {
        let mut out = vec![];
        self.touch(&[], &mut out);
        if let Some(mut p) = self.pointer {
            p.buttons = 0;
            self.pointer(&p, &mut out);
        }
        for code in std::mem::take(&mut self.held) {
            out.push(Event::Key { code, down: false });
        }
        let idx: Vec<u8> = self.pads.keys().copied().collect();
        for i in idx {
            self.pad(&neutral(i), &mut out);
        }
        out
    }

    fn touch(&mut self, cs: &[Contact], out: &mut Vec<Event>) {
        let now: BTreeMap<u8, (u16, u16)> = cs
            .iter()
            .filter(|c| c.tool == Tool::Finger)
            .map(|c| (c.id, (c.x, c.y)))
            .collect();
        for id in self.fingers.keys() {
            if !now.contains_key(id) {
                out.push(Event::TouchUp { slot: *id as u32 });
            }
        }
        for (id, raw) in &now {
            let (slot, x, y) = (*id as u32, norm(raw.0), norm(raw.1));
            match self.fingers.get(id) {
                None => out.push(Event::TouchDown { slot, x, y }),
                Some(old) if old != raw => out.push(Event::TouchMotion { slot, x, y }),
                Some(_) => {}
            }
        }
        self.fingers = now;
        // One stylus at a time: the first one listed.
        let pen = cs.iter().find(|c| c.tool != Tool::Finger).map(|c| Pen {
            x: norm(c.x),
            y: norm(c.y),
            pressure: if c.hover {
                0.0
            } else if c.pressure == 0 {
                0.5
            } else {
                norm(c.pressure)
            },
            touching: !c.hover,
            eraser: c.tool == Tool::Eraser,
            button: c.button,
        });
        match (self.pen, pen) {
            (Some(_), None) => out.push(Event::PenOut),
            (old, Some(p)) if old != Some(p) => out.push(Event::Pen(p)),
            _ => {}
        }
        self.pen = pen;
    }

    fn pointer(&mut self, p: &wire::Pointer, out: &mut Vec<Event>) {
        let Some(old) = self.pointer.replace(*p) else {
            // The first record only sets where the running totals start; its position and buttons still count.
            if let Some((x, y)) = p.abs {
                out.push(Event::PointerAbs {
                    x: norm(x),
                    y: norm(y),
                });
            }
            for (bit, code) in BUTTONS.iter().enumerate() {
                if p.buttons & 1 << bit != 0 {
                    out.push(Event::Button {
                        code: *code,
                        down: true,
                    });
                }
            }
            return;
        };
        if let Some((x, y)) = p.abs.filter(|a| Some(*a) != old.abs) {
            out.push(Event::PointerAbs {
                x: norm(x),
                y: norm(y),
            });
        }
        let (dx, dy) = (
            p.rel.0.wrapping_sub(old.rel.0),
            p.rel.1.wrapping_sub(old.rel.1),
        );
        if dx != 0 || dy != 0 {
            out.push(Event::PointerRel {
                dx: dx as f64 / 16.0,
                dy: dy as f64 / 16.0,
            });
        }
        let (wy, wx) = (
            p.wheel.0.wrapping_sub(old.wheel.0),
            p.wheel.1.wrapping_sub(old.wheel.1),
        );
        if p.smooth {
            let px = |v: i32| v as f64 / 120.0 * PX_PER_NOTCH;
            if wy != 0 || wx != 0 {
                self.scrolling = true;
                out.push(Event::Scroll {
                    dx: px(wx),
                    dy: px(wy),
                    finish: false,
                });
            } else if self.scrolling {
                // A repeat with the same totals: the fingers stopped or left.
                self.scrolling = false;
                out.push(Event::Scroll {
                    dx: 0.0,
                    dy: 0.0,
                    finish: true,
                });
            }
        } else if wy != 0 || wx != 0 {
            // Whole notches only; a high-resolution wheel's part-notches wait for the rest.
            let (ry, rx) = (self.wheel_rest.0 + wy, self.wheel_rest.1 + wx);
            let (vertical, horizontal) = (ry / 120, rx / 120);
            self.wheel_rest = (ry % 120, rx % 120);
            if vertical != 0 || horizontal != 0 {
                out.push(Event::Notches {
                    vertical,
                    horizontal,
                });
            }
        }
        for (bit, code) in BUTTONS.iter().enumerate() {
            let (was, is) = (old.buttons & 1 << bit != 0, p.buttons & 1 << bit != 0);
            if was != is {
                out.push(Event::Button {
                    code: *code,
                    down: is,
                });
            }
        }
    }

    fn keys(&mut self, k: &wire::Keys, out: &mut Vec<Event>) {
        for (i, (code, down)) in k.events.iter().enumerate() {
            let n = k.first.wrapping_add(i as u16);
            if self.keys_applied.is_some_and(|a| !wire::newer16(n, a)) {
                continue;
            }
            self.keys_applied = Some(n);
            let changed = if *down {
                self.held.insert(*code)
            } else {
                self.held.remove(code)
            };
            if changed {
                out.push(Event::Key {
                    code: *code,
                    down: *down,
                });
            }
        }
        // Up events lost with every repeat of them: the held list lets those keys go.
        let stale: Vec<u16> = self
            .held
            .iter()
            .filter(|c| !k.held.contains(c))
            .copied()
            .collect();
        for code in stale {
            self.held.remove(&code);
            out.push(Event::Key { code, down: false });
        }
    }

    fn pad(&mut self, g: &Pad, out: &mut Vec<Event>) {
        if !g.connected {
            if self.pads.remove(&g.index).is_some() {
                out.push(Event::PadGone(g.index));
            }
            return;
        }
        if self.pads.insert(g.index, *g) != Some(*g) {
            out.push(Event::Pad(*g));
        }
    }
}

fn neutral(index: u8) -> Pad {
    Pad {
        index,
        connected: true,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::{Keys, Pointer};

    const TOKEN: u64 = 42;

    fn pkt(seq: u32, records: Vec<Record>) -> Packet {
        Packet {
            seq,
            time_us: 0,
            token: TOKEN,
            records,
        }
    }

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

    #[test]
    fn multitouch_downs_moves_and_ups_from_states() {
        let mut t = Tracker::new(TOKEN);
        let now = Instant::now();
        let ev = t.apply(
            &pkt(
                1,
                vec![Record::Touch(vec![
                    finger(0, 0, 0),
                    finger(1, 65535, 65535),
                ])],
            ),
            now,
        );
        assert_eq!(
            ev,
            vec![
                Event::TouchDown {
                    slot: 0,
                    x: 0.0,
                    y: 0.0
                },
                Event::TouchDown {
                    slot: 1,
                    x: 1.0,
                    y: 1.0
                }
            ]
        );
        // A repeat of the same state is nothing new.
        assert!(t
            .apply(
                &pkt(
                    2,
                    vec![Record::Touch(vec![
                        finger(0, 0, 0),
                        finger(1, 65535, 65535)
                    ])]
                ),
                now
            )
            .is_empty());
        // Finger 0 lifted and finger 1 moved, in one lost-and-found state.
        let ev = t.apply(&pkt(5, vec![Record::Touch(vec![finger(1, 0, 65535)])]), now);
        assert_eq!(
            ev,
            vec![
                Event::TouchUp { slot: 0 },
                Event::TouchMotion {
                    slot: 1,
                    x: 0.0,
                    y: 1.0
                }
            ]
        );
        assert!(t.busy());
        assert_eq!(
            t.apply(&pkt(6, vec![Record::Touch(vec![])]), now),
            vec![Event::TouchUp { slot: 1 }]
        );
        assert!(!t.busy());
    }

    #[test]
    fn late_repeated_and_foreign_datagrams_do_nothing() {
        let mut t = Tracker::new(TOKEN);
        let now = Instant::now();
        assert_eq!(
            t.apply(&pkt(10, vec![Record::Touch(vec![finger(0, 1, 1)])]), now)
                .len(),
            1
        );
        assert!(
            t.apply(&pkt(9, vec![Record::Touch(vec![])]), now)
                .is_empty(),
            "late"
        );
        assert!(
            t.apply(&pkt(10, vec![Record::Touch(vec![])]), now)
                .is_empty(),
            "repeated"
        );
        let mut foreign = pkt(11, vec![Record::Touch(vec![])]);
        foreign.token = 7;
        assert!(t.apply(&foreign, now).is_empty(), "wrong token");
        assert_eq!(
            t.apply(&pkt(11, vec![Record::Touch(vec![])]), now),
            vec![Event::TouchUp { slot: 0 }]
        );
    }

    #[test]
    fn stylus_pressure_hover_and_leaving() {
        let mut t = Tracker::new(TOKEN);
        let now = Instant::now();
        let pen = |hover, pressure| Contact {
            id: 0,
            tool: Tool::Stylus,
            button: true,
            hover,
            x: 65535,
            y: 0,
            pressure,
        };
        let ev = t.apply(&pkt(1, vec![Record::Touch(vec![pen(true, 0)])]), now);
        assert_eq!(
            ev,
            vec![Event::Pen(Pen {
                x: 1.0,
                y: 0.0,
                pressure: 0.0,
                touching: false,
                eraser: false,
                button: true
            })]
        );
        assert!(!t.busy(), "hovering holds nothing");
        let ev = t.apply(&pkt(2, vec![Record::Touch(vec![pen(false, 65535)])]), now);
        assert!(
            matches!(ev[..], [Event::Pen(Pen { touching: true, pressure, .. })] if pressure == 1.0)
        );
        assert_eq!(
            t.apply(&pkt(3, vec![Record::Touch(vec![])]), now),
            vec![Event::PenOut]
        );
    }

    #[test]
    fn pointer_running_totals_survive_a_lost_datagram() {
        let mut t = Tracker::new(TOKEN);
        let now = Instant::now();
        let p = |rel, buttons| {
            Record::Pointer(Pointer {
                buttons,
                rel,
                ..Default::default()
            })
        };
        assert!(
            t.apply(&pkt(1, vec![p((160, 0), 0)]), now).is_empty(),
            "the first record sets the base"
        );
        // Datagram 2 (+16 px, left down) is lost; 3 has the totals and the button.
        let ev = t.apply(&pkt(3, vec![p((160 + 512, -32), 1)]), now);
        assert_eq!(
            ev,
            vec![
                Event::PointerRel { dx: 32.0, dy: -2.0 },
                Event::Button {
                    code: BTN_LEFT,
                    down: true
                }
            ]
        );
        assert_eq!(
            t.apply(&pkt(4, vec![p((672, -32), 0)]), now),
            vec![Event::Button {
                code: BTN_LEFT,
                down: false
            }]
        );
        // Totals wrap around i32 without a jump.
        let mut t = Tracker::new(TOKEN);
        t.apply(&pkt(1, vec![p((i32::MAX, 0), 0)]), now);
        assert_eq!(
            t.apply(&pkt(2, vec![p((i32::MIN, 0), 0)]), now),
            vec![Event::PointerRel {
                dx: 1.0 / 16.0,
                dy: 0.0
            }]
        );
    }

    #[test]
    fn wheel_notches_and_smooth_scrolling() {
        let mut t = Tracker::new(TOKEN);
        let now = Instant::now();
        let w = |wheel, smooth| {
            Record::Pointer(Pointer {
                wheel,
                smooth,
                ..Default::default()
            })
        };
        t.apply(&pkt(1, vec![w((0, 0), false)]), now);
        assert!(
            t.apply(&pkt(2, vec![w((60, 0), false)]), now).is_empty(),
            "half a notch waits"
        );
        assert_eq!(
            t.apply(&pkt(3, vec![w((240, -120), false)]), now),
            vec![Event::Notches {
                vertical: 2,
                horizontal: -1
            }]
        );
        let ev = t.apply(&pkt(4, vec![w((480, -120), true)]), now);
        assert_eq!(
            ev,
            vec![Event::Scroll {
                dx: 0.0,
                dy: 2.0 * PX_PER_NOTCH,
                finish: false
            }]
        );
        assert_eq!(
            t.apply(&pkt(5, vec![w((480, -120), true)]), now),
            vec![Event::Scroll {
                dx: 0.0,
                dy: 0.0,
                finish: true
            }]
        );
        assert!(t.apply(&pkt(6, vec![w((480, -120), true)]), now).is_empty());
    }

    #[test]
    fn key_events_apply_once_and_held_heals_lost_ups() {
        let mut t = Tracker::new(TOKEN);
        let now = Instant::now();
        let k = |first, events: Vec<(u16, bool)>, held: Vec<u16>| {
            Record::Keys(Keys {
                first,
                events,
                held,
            })
        };
        let ev = t.apply(
            &pkt(
                1,
                vec![k(65535, vec![(42, true), (30, true)], vec![42, 30])],
            ),
            now,
        );
        assert_eq!(
            ev,
            vec![
                Event::Key {
                    code: 42,
                    down: true
                },
                Event::Key {
                    code: 30,
                    down: true
                }
            ]
        );
        // The same events repeated, plus a new one numbered across the u16 wrap.
        let ev = t.apply(
            &pkt(
                2,
                vec![k(
                    65535,
                    vec![(42, true), (30, true), (30, false)],
                    vec![42],
                )],
            ),
            now,
        );
        assert_eq!(
            ev,
            vec![Event::Key {
                code: 30,
                down: false
            }]
        );
        // Every repeat of shift's release was lost: the held list lets it go.
        let ev = t.apply(&pkt(9, vec![k(10, vec![], vec![])]), now);
        assert_eq!(
            ev,
            vec![Event::Key {
                code: 42,
                down: false
            }]
        );
        assert!(!t.busy());
    }

    #[test]
    fn gamepads_change_and_go() {
        let mut t = Tracker::new(TOKEN);
        let now = Instant::now();
        let pad = Pad {
            index: 0,
            connected: true,
            buttons: 1,
            left: (100, -100),
            ..Default::default()
        };
        assert_eq!(
            t.apply(&pkt(1, vec![Record::Gamepad(pad)]), now),
            vec![Event::Pad(pad)]
        );
        assert!(t.apply(&pkt(2, vec![Record::Gamepad(pad)]), now).is_empty());
        assert!(t.busy());
        let gone = Pad {
            index: 0,
            connected: false,
            ..Default::default()
        };
        assert_eq!(
            t.apply(&pkt(3, vec![Record::Gamepad(gone)]), now),
            vec![Event::PadGone(0)]
        );
    }

    #[test]
    fn watchdog_lets_go_after_half_a_second_of_silence() {
        let mut t = Tracker::new(TOKEN);
        let t0 = Instant::now();
        t.apply(
            &pkt(
                1,
                vec![
                    Record::Touch(vec![finger(4, 1, 1)]),
                    Record::Pointer(Pointer {
                        buttons: 2,
                        ..Default::default()
                    }),
                    Record::Keys(Keys {
                        first: 0,
                        events: vec![(29, true)],
                        held: vec![29],
                    }),
                    Record::Gamepad(Pad {
                        index: 2,
                        connected: true,
                        triggers: (9, 0),
                        ..Default::default()
                    }),
                ],
            ),
            t0,
        );
        assert!(t.watchdog(t0 + Duration::from_millis(499)).is_empty());
        let ev = t.watchdog(t0 + WATCHDOG);
        assert_eq!(
            ev,
            vec![
                Event::TouchUp { slot: 4 },
                Event::Button {
                    code: 0x111,
                    down: false
                },
                Event::Key {
                    code: 29,
                    down: false
                },
                Event::Pad(neutral(2)),
            ]
        );
        assert!(!t.busy());
        assert!(t.watchdog(t0 + WATCHDOG * 3).is_empty(), "once");
    }
}
