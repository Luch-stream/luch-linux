//! The receiver's input on this computer (PROTOCOL.md 4h, docs/input.md): datagrams on the video flow, the
//! person's permission, injection through the RemoteDesktop portal or uinput.

pub mod ask;
pub mod inject;
pub mod map;
pub mod portal;
pub mod state;
pub mod uinput;
pub mod wire;

#[cfg(test)]
mod fake_portal;
#[cfg(test)]
mod tests;

use std::{
    net::UdpSocket,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use serde_json::{json, Value};
use std::time::Instant;
use tokio::sync::mpsc;

pub use ask::Answer;
use inject::Injector;
use state::Tracker;

/// One session's input: the receiving thread, the injecting task and whether the person allowed it.
pub struct Input {
    pub token: u64,
    granted: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    accept: Vec<&'static str>,
    area: (u32, u32),
    /// The person's answer in this session, once given.
    answered: Option<Answer>,
    asking: bool,
}

/// What to do with an `input_request`.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// Answer the receiver with this message right away.
    Reply(Value),
    /// Ask the person, then call `answer`.
    Ask,
    /// A question is already on the screen.
    Wait,
}

impl Input {
    /// Listens on (a clone of) the session's video socket: input comes back on the flow the video goes out on.
    /// Nothing is injected before `grant(true)`.
    pub fn start(video: &UdpSocket, injector: Injector) -> std::io::Result<Self> {
        let sock = video.try_clone()?;
        // A timeout, not O_NONBLOCK: that flag would be shared with the encoder thread's sending socket.
        sock.set_read_timeout(Some(Duration::from_millis(100)))?;
        let token = u64::from_be_bytes(uuid::Uuid::new_v4().as_bytes()[..8].try_into().unwrap());
        let (granted, stop) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (accept, area) = (injector.accepts(), injector.geometry().area_size());
        let (tx, rx) = mpsc::channel::<wire::Packet>(64);
        let st = stop.clone();
        std::thread::Builder::new()
            .name("luch-input".into())
            .spawn(move || {
                let mut buf = [0u8; 2048];
                while !st.load(Ordering::Relaxed) {
                    match sock.recv(&mut buf) {
                        Ok(n) => match wire::parse(&buf[..n]) {
                            // A full queue means the injector is behind: the newer state will follow.
                            Ok(p) => {
                                let _ = tx.try_send(p);
                            }
                            Err(e) => eprintln!("[luchd] input: dropped a datagram ({e})"),
                        },
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        // ECONNREFUSED from an ICMP error on the connected socket: the receiver is gone for a moment.
                        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
                        Err(e) => {
                            eprintln!("[luchd] input: {e}");
                            break;
                        }
                    }
                }
            })?;
        tokio::spawn(run(rx, Tracker::new(token), injector, granted.clone()));
        Ok(Input {
            token,
            granted,
            stop,
            accept,
            area,
            answered: None,
            asking: false,
        })
    }

    pub fn can_inject(&self) -> bool {
        !self.accept.is_empty()
    }

    pub fn grant(&self, on: bool) {
        self.granted.store(on, Ordering::Relaxed);
    }

    /// At the start of a session: a receiver the person said "always" to is granted without a request.
    pub fn remembered(&mut self, always: bool) -> Option<Value> {
        (always && self.can_inject()).then(|| self.answer(Answer::Always))
    }

    /// The receiver asks for control (PROTOCOL.md 4h): the answer given before, a no if nothing can be
    /// injected here, or a question for the person.
    pub fn request(&mut self) -> Step {
        if !self.can_inject() {
            self.answered = Some(Answer::No);
        }
        match self.answered {
            Some(a) => Step::Reply(self.message(a)),
            None if self.asking => Step::Wait,
            None => {
                self.asking = true;
                Step::Ask
            }
        }
    }

    /// The person answered: the gate opens (or stays shut) and this goes to the receiver.
    pub fn answer(&mut self, a: Answer) -> Value {
        self.asking = false;
        self.answered = Some(a);
        self.grant(a != Answer::No && self.can_inject());
        self.message(a)
    }

    /// The `input` control message for an answer.
    pub fn message(&self, a: Answer) -> Value {
        if a == Answer::No || !self.can_inject() {
            return json!({"t": "input", "grant": "no"});
        }
        json!({"t": "input", "grant": a.grant(), "token": format!("{:016x}", self.token),
               "accept": self.accept, "area": [self.area.0, self.area.1]})
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        // The thread ends within its read timeout; the task then lets everything go.
        self.stop.store(true, Ordering::Relaxed);
    }
}

async fn run(
    mut rx: mpsc::Receiver<wire::Packet>,
    mut tracker: Tracker,
    mut inj: Injector,
    granted: Arc<AtomicBool>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        let events = tokio::select! {
            p = rx.recv() => match p {
                Some(p) if granted.load(Ordering::Relaxed) => tracker.apply(&p, Instant::now()),
                Some(_) => vec![],
                None => break,
            },
            _ = tick.tick() => {
                if granted.load(Ordering::Relaxed) {
                    tracker.watchdog(Instant::now())
                } else {
                    // Taken back with something held: let it go.
                    tracker.release_all()
                }
            }
        };
        for e in events {
            inj.inject(e).await;
        }
    }
    for e in tracker.release_all() {
        inj.inject(e).await;
    }
}

/// The input part of a session, as `luchd` decides it before the capture starts.
pub struct Config {
    pub dbus: zbus::Connection,
    /// The receiver said "input" in its welcome.
    pub receiver_can: bool,
    /// The person said "always" for this receiver before.
    pub always: bool,
}

/// Where input may come from at all: `LUCH_INPUT=off` turns it off, `uinput` skips the portal.
pub fn mode() -> &'static str {
    match std::env::var("LUCH_INPUT").as_deref() {
        Ok("off") => "off",
        Ok("uinput") => "uinput",
        Ok("portal") => "portal",
        _ => "auto",
    }
}
