//! The Luch protocol as every implementation shares it: constants, the control channel's lines and messages
//! (`control`), the media datagram (`wire`) and the session numbers (`session`). No platform code: it builds for
//! Linux, Windows and anything else Rust targets.

pub mod control;
pub mod session;
pub mod wire;

/// The plain control port's version (older receivers); `VERSION_TLS` on the TLS port, with pairing (PROTOCOL.md 3a).
pub const VERSION: u32 = 0;
pub const VERSION_TLS: u32 = 1;
pub const RECEIVER_TYPE: &str = "_luch._tcp";
pub const SENDER_TYPE: &str = "_luchsrc._tcp";
pub const CONTROL_PORT: u16 = 47800;
pub const WEB_PORT: u16 = 47801; // built-in browser receiver: page + websocket
pub const MAX_LINE: usize = 64 * 1024;
pub const APP: &str = concat!("luchd/", env!("CARGO_PKG_VERSION"));

pub const DBUS_NAME: &str = "dev.luch.Luch1";
pub const DBUS_PATH: &str = "/dev/luch/Luch1";

/// D-Bus row: id, name, model, address, ifname, port, connected, mirroring.
pub type ReceiverRow = (String, String, String, String, String, u16, bool, bool);
