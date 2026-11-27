//! Do not disturb while presenting or mirroring: GNOME's notification banners off, and back as they were afterwards.
//!
//! The setting is "ask" (the default), "on" or "off". On "ask" luchd cannot ask by itself: it signals `QuietAsk`
//! and the GNOME extension asks once; the answer holds until luchd restarts (the login session). What was changed is
//! written down first, so a crash mid-session is undone at the next start, as with the sound (mirror.rs).

use std::sync::Mutex;

const KEY: (&str, &str) = ("org.gnome.desktop.notifications", "show-banners");

/// Where the banners switch lives; GNOME's gsettings, or a stand-in in tests.
pub trait Banners: Send {
    /// Whether banners are shown now; None when there is no such setting (not GNOME).
    fn shown(&self) -> Option<bool>;
    fn set(&mut self, on: bool);
}

pub struct GSettings;

impl Banners for GSettings {
    fn shown(&self) -> Option<bool> {
        let o = std::process::Command::new("gsettings")
            .args(["get", KEY.0, KEY.1])
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        match String::from_utf8_lossy(&o.stdout).trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    fn set(&mut self, on: bool) {
        let _ = std::process::Command::new("gsettings")
            .args(["set", KEY.0, KEY.1, if on { "true" } else { "false" }])
            .output();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Ask,
    On,
    Off,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim() {
            "ask" => Some(Mode::Ask),
            "on" => Some(Mode::On),
            "off" => Some(Mode::Off),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Ask => "ask",
            Mode::On => "on",
            Mode::Off => "off",
        }
    }
}

/// What a session start asks of the caller.
#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    Nothing,
    /// The extension should ask the person (signal `QuietAsk`).
    Ask,
}

pub struct Quiet {
    banners: Box<dyn Banners>,
    /// The file that says banners were on before we turned them off; None: nothing is written (tests).
    note: Option<std::path::PathBuf>,
    mode: Mode,
    answer: Option<bool>,
    sessions: usize,
    /// Banners were shown and we turned them off.
    hid: bool,
}

impl Quiet {
    pub fn new(banners: Box<dyn Banners>, note: Option<std::path::PathBuf>, mode: Mode) -> Quiet {
        Quiet {
            banners,
            note,
            mode,
            answer: None,
            sessions: 0,
            hid: false,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.apply();
    }

    /// A presentation or a mirroring session began.
    pub fn start(&mut self) -> Start {
        self.sessions += 1;
        self.apply();
        if self.mode == Mode::Ask && self.answer.is_none() {
            Start::Ask
        } else {
            Start::Nothing
        }
    }

    pub fn end(&mut self) {
        self.sessions = self.sessions.saturating_sub(1);
        self.apply();
    }

    /// The person's answer to the question: hide notifications (true) or not; kept for the rest of the login.
    pub fn answer(&mut self, hide: bool) {
        self.answer = Some(hide);
        self.apply();
    }

    fn wanted(&self) -> bool {
        self.sessions > 0
            && match self.mode {
                Mode::On => true,
                Mode::Off => false,
                Mode::Ask => self.answer == Some(true),
            }
    }

    /// Banners off while wanted, back on (only if we turned them off) when not.
    fn apply(&mut self) {
        if self.wanted() && !self.hid {
            if self.banners.shown() == Some(true) {
                // Written before the change: a crash right after it is still undone.
                if let Some(n) = &self.note {
                    let _ = std::fs::write(n, "true");
                }
                self.banners.set(false);
                self.hid = true;
            }
        } else if !self.wanted() && self.hid {
            self.banners.set(true);
            self.hid = false;
            if let Some(n) = &self.note {
                let _ = std::fs::remove_file(n);
            }
        }
    }

    /// At start: banners back on if a crash left them off.
    pub fn recover(&mut self) {
        if let Some(n) = &self.note {
            if std::fs::read_to_string(n).is_ok_and(|t| t.trim() == "true") {
                self.banners.set(true);
            }
            let _ = std::fs::remove_file(n);
        }
    }
}

impl Drop for Quiet {
    fn drop(&mut self) {
        self.sessions = 0;
        self.apply();
    }
}

static QUIET: Mutex<Option<Quiet>> = Mutex::new(None);

/// Runs `f` on the daemon's one Quiet, made on first use from the saved setting (and a crash's leftovers undone).
pub fn with<R>(f: impl FnOnce(&mut Quiet) -> R) -> R {
    let mut g = QUIET.lock().unwrap_or_else(|e| e.into_inner());
    let q = g.get_or_insert_with(|| {
        let dir = crate::config_dir().ok();
        let mode = dir
            .as_ref()
            .and_then(|d| std::fs::read_to_string(d.join("quiet")).ok())
            .and_then(|s| Mode::parse(&s))
            .unwrap_or(Mode::Ask);
        let mut q = Quiet::new(
            Box::new(GSettings),
            dir.map(|d| d.join("restore-banners")),
            mode,
        );
        q.recover();
        q
    });
    f(q)
}

pub fn save_mode(mode: Mode) {
    if let Ok(d) = crate::config_dir() {
        let _ = std::fs::write(d.join("quiet"), mode.as_str());
    }
}

/// Held while a session runs: counts it in, and out when dropped, however the session ends.
pub struct Session;

impl Session {
    /// Starts counting; true when the person should be asked.
    pub fn start() -> (Session, bool) {
        let ask = with(|q| q.start()) == Start::Ask;
        (Session, ask)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        with(|q| q.end());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Fake(Arc<Mutex<Option<bool>>>, Arc<Mutex<u32>>);

    impl Banners for Fake {
        fn shown(&self) -> Option<bool> {
            *self.0.lock().unwrap()
        }
        fn set(&mut self, on: bool) {
            *self.0.lock().unwrap() = Some(on);
            *self.1.lock().unwrap() += 1;
        }
    }

    fn fake(on: Option<bool>) -> Fake {
        Fake(Arc::new(Mutex::new(on)), Arc::new(Mutex::new(0)))
    }

    fn note() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "luch-quiet-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d.join("restore-banners")
    }

    #[test]
    fn on_hides_banners_for_the_session_and_restores_them() {
        let b = fake(Some(true));
        let n = note();
        let mut q = Quiet::new(Box::new(b.clone()), Some(n.clone()), Mode::On);
        assert_eq!(q.start(), Start::Nothing);
        assert_eq!(b.shown(), Some(false));
        assert!(n.exists(), "written down before the change");
        q.start(); // a second session at once
        q.end();
        assert_eq!(b.shown(), Some(false), "still one session running");
        q.end();
        assert_eq!(b.shown(), Some(true));
        assert!(!n.exists());
    }

    #[test]
    fn ask_asks_once_and_follows_the_answer() {
        let b = fake(Some(true));
        let mut q = Quiet::new(Box::new(b.clone()), None, Mode::Ask);
        assert_eq!(q.start(), Start::Ask);
        assert_eq!(b.shown(), Some(true), "nothing changes before the answer");
        q.answer(true);
        assert_eq!(b.shown(), Some(false));
        q.end();
        assert_eq!(b.shown(), Some(true));
        assert_eq!(q.start(), Start::Nothing, "asked once per login");
        assert_eq!(b.shown(), Some(false));
        q.end();
        q.answer(false);
        assert_eq!(q.start(), Start::Nothing);
        assert_eq!(b.shown(), Some(true));
    }

    #[test]
    fn banners_already_off_or_missing_are_left_alone() {
        let b = fake(Some(false));
        let mut q = Quiet::new(Box::new(b.clone()), None, Mode::On);
        q.start();
        q.end();
        assert_eq!(
            (b.shown(), *b.1.lock().unwrap()),
            (Some(false), 0),
            "the person's own choice stays"
        );
        let none = fake(None);
        let mut q = Quiet::new(Box::new(none.clone()), None, Mode::On);
        q.start();
        drop(q);
        assert_eq!(*none.1.lock().unwrap(), 0, "not GNOME: nothing is set");
    }

    #[test]
    fn turning_it_off_mid_session_restores_at_once() {
        let b = fake(Some(true));
        let mut q = Quiet::new(Box::new(b.clone()), None, Mode::On);
        q.start();
        q.set_mode(Mode::Off);
        assert_eq!(b.shown(), Some(true));
    }

    #[test]
    fn a_crash_left_banners_off_and_the_next_start_turns_them_on() {
        let b = fake(Some(true));
        let n = note();
        let mut q = Quiet::new(Box::new(b.clone()), Some(n.clone()), Mode::On);
        q.start();
        std::mem::forget(q); // the daemon dies: no Drop
        assert_eq!(b.shown(), Some(false));
        let mut again = Quiet::new(Box::new(b.clone()), Some(n.clone()), Mode::On);
        again.recover();
        assert_eq!(b.shown(), Some(true));
        assert!(!n.exists());
    }
}
