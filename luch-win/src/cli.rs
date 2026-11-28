//! Command line of `luch-win`. Discovery (mDNS) comes later; for now the receiver's address is given.

use std::net::{Ipv4Addr, SocketAddrV4};

use luch_proto::CONTROL_PORT;

use crate::shell;

pub const USAGE: &str = "usage: luch-win [mirror] --to IP[:PORT] [--from LOCAL_IP] [--output N] [--codec avc|hevc] [--size WxH|auto] [--watch-device]
       luch-win cast --to IP[:PORT] [--from LOCAL_IP] FILE...   photos (a slideshow), a video, a sound or a PDF
       luch-win install-shell [--to IP[:PORT] [--name NAME]]   Show on TV in Explorer's menu and in Send To
       luch-win uninstall-shell
  --watch-device: watch the TV's heat and battery (a badge on a hot TV; off by default).
  The TV asks its person to Allow this PC each time (no pairing from Windows yet). install-shell adds a receiver
  each time it is run with --to; without, it writes the menus again (after luch-win.exe was moved).";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cast {
    pub to: SocketAddrV4,
    pub from: Option<Ipv4Addr>,
    pub files: Vec<String>,
    /// Started from Explorer's menu or Send To: no console, refusals in a message box, one selection gathered.
    pub shell: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Mirror(Options),
    Cast(Cast),
    InstallShell(Option<shell::Receiver>),
    UninstallShell,
}

pub fn command(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut args: Vec<String> = args.into_iter().collect();
    match args.first().map(String::as_str) {
        Some("cast") => {
            let (mut to, mut from, mut files, mut from_shell) = (None, None, Vec::new(), false);
            let mut it = args.drain(1..);
            while let Some(a) = it.next() {
                let mut v = || it.next().ok_or(format!("{a} needs a value"));
                match a.as_str() {
                    "--to" => to = Some(shell::address(&v()?)?),
                    "--from" => {
                        from = Some(v()?.parse().map_err(|_| "--from: not an IPv4 address")?)
                    }
                    "--shell" => from_shell = true,
                    o if o.starts_with("--") => return Err(format!("unknown option {o}\n{USAGE}")),
                    _ => files.push(a),
                }
            }
            if files.is_empty() {
                return Err(format!("cast what? a file\n{USAGE}"));
            }
            Ok(Command::Cast(Cast {
                to: to.ok_or(format!("cast needs --to\n{USAGE}"))?,
                from,
                files,
                shell: from_shell,
            }))
        }
        Some("install-shell") => {
            let (mut to, mut name) = (None, None);
            let mut it = args.drain(1..);
            while let Some(a) = it.next() {
                let v = it.next().ok_or(format!("{a} needs a value"))?;
                match a.as_str() {
                    "--to" => to = Some(shell::address(&v)?),
                    "--name" => name = Some(v),
                    o => return Err(format!("unknown option {o}\n{USAGE}")),
                }
            }
            Ok(Command::InstallShell(match (to, name) {
                (Some(to), name) => Some(shell::Receiver {
                    name: name.unwrap_or_else(|| format!("TV {}", to.ip())),
                    to,
                }),
                (None, None) => None,
                (None, Some(_)) => return Err("--name goes with --to".into()),
            }))
        }
        Some("uninstall-shell") if args.len() == 1 => Ok(Command::UninstallShell),
        Some("mirror") => parse(args.into_iter().skip(1)).map(Command::Mirror),
        _ => parse(args).map(Command::Mirror),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The receiver's control port.
    pub to: SocketAddrV4,
    /// The address of the LAN interface to send from (PROTOCOL.md section 2).
    pub from: Option<Ipv4Addr>,
    /// Which monitor, in DXGI order over all adapters.
    pub output: u32,
    /// "avc", "hevc", or "" to let the receiver's bench decide.
    pub codec: String,
    /// As LUCH_SIZE for luchd: unset, `auto`, or `WxH`.
    pub size: Option<String>,
    /// Ask for the receiver's heat badge (PROTOCOL.md 5); off by default.
    pub watch_device: bool,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut to = None;
    let mut o = Options {
        to: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
        from: None,
        output: 0,
        codec: String::new(),
        size: None,
        watch_device: false,
    };
    let mut it = args.into_iter();
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or(format!("{k} needs a value"));
        match k.as_str() {
            "--to" => {
                let v = v()?;
                to = Some(match v.parse::<SocketAddrV4>() {
                    Ok(a) => a,
                    Err(_) => SocketAddrV4::new(
                        v.parse()
                            .map_err(|_| format!("--to: not an IPv4 address: {v}"))?,
                        CONTROL_PORT,
                    ),
                });
            }
            "--from" => o.from = Some(v()?.parse().map_err(|_| "--from: not an IPv4 address")?),
            "--output" => o.output = v()?.parse().map_err(|_| "--output: not a number")?,
            "--codec" => {
                o.codec = v()?;
                if o.codec != "avc" && o.codec != "hevc" {
                    return Err("--codec is avc or hevc".into());
                }
            }
            "--size" => o.size = Some(v()?),
            "--watch-device" => o.watch_device = true,
            _ => return Err(format!("unknown option {k}\n{USAGE}")),
        }
    }
    o.to = to.ok_or(USAGE)?;
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn the_receiver_is_needed_and_its_port_defaults() {
        assert!(parse(args("")).is_err());
        let o = parse(args("--to 192.168.1.20")).unwrap();
        assert_eq!(o.to, "192.168.1.20:47800".parse().unwrap());
        assert_eq!((o.output, o.codec.as_str(), o.from), (0, "", None));
        assert!(!o.watch_device, "off by default");
    }

    #[test]
    fn every_option() {
        let o = parse(args(
            "--to 192.168.1.20:5000 --from 192.168.1.10 --output 1 --codec hevc --size auto --watch-device",
        ))
        .unwrap();
        assert_eq!(o.to.port(), 5000);
        assert_eq!(o.from, Some(Ipv4Addr::new(192, 168, 1, 10)));
        assert_eq!((o.output, o.codec.as_str()), (1, "hevc"));
        assert_eq!(o.size.as_deref(), Some("auto"));
        assert!(o.watch_device);
    }

    #[test]
    fn the_commands() {
        assert_eq!(
            command(args("mirror --to 192.168.1.20")).unwrap(),
            command(args("--to 192.168.1.20")).unwrap()
        );
        let Command::Cast(c) =
            command(args("cast --shell --to 192.168.1.20 C:\\a.jpg C:\\b.jpg")).unwrap()
        else {
            panic!()
        };
        assert_eq!(c.to, "192.168.1.20:47800".parse().unwrap());
        assert!(c.shell);
        assert_eq!(c.files, ["C:\\a.jpg", "C:\\b.jpg"]);
        assert!(command(args("cast --to 192.168.1.20")).is_err());
        assert!(command(args("cast a.jpg")).is_err());
        assert_eq!(
            command(args("install-shell --to 192.168.1.20:5000 --name Den")).unwrap(),
            Command::InstallShell(Some(shell::Receiver {
                name: "Den".into(),
                to: "192.168.1.20:5000".parse().unwrap()
            }))
        );
        let Command::InstallShell(Some(r)) =
            command(args("install-shell --to 192.168.1.20")).unwrap()
        else {
            panic!()
        };
        assert_eq!(r.name, "TV 192.168.1.20");
        assert_eq!(
            command(args("install-shell")).unwrap(),
            Command::InstallShell(None)
        );
        assert!(command(args("install-shell --name Den")).is_err());
        assert_eq!(
            command(args("uninstall-shell")).unwrap(),
            Command::UninstallShell
        );
    }

    #[test]
    fn bad_values_are_refused() {
        assert!(parse(args("--to tv.local")).is_err());
        assert!(parse(args("--to 192.168.1.20 --codec av1")).is_err());
        assert!(parse(args("--to 192.168.1.20 --output")).is_err());
        assert!(parse(args("--to 192.168.1.20 --loud")).is_err());
    }
}
