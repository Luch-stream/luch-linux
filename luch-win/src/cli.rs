//! Command line of `luch-win`. Discovery (mDNS) comes later; for now the receiver's address is given.

use std::net::{Ipv4Addr, SocketAddrV4};

use luch_proto::CONTROL_PORT;

pub const USAGE: &str = "usage: luch-win --to IP[:PORT] [--from LOCAL_IP] [--output N] [--codec avc|hevc] [--size WxH|auto]";

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
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut to = None;
    let mut o = Options {
        to: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
        from: None,
        output: 0,
        codec: String::new(),
        size: None,
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
    }

    #[test]
    fn every_option() {
        let o = parse(args(
            "--to 192.168.1.20:5000 --from 192.168.1.10 --output 1 --codec hevc --size auto",
        ))
        .unwrap();
        assert_eq!(o.to.port(), 5000);
        assert_eq!(o.from, Some(Ipv4Addr::new(192, 168, 1, 10)));
        assert_eq!((o.output, o.codec.as_str()), (1, "hevc"));
        assert_eq!(o.size.as_deref(), Some("auto"));
    }

    #[test]
    fn bad_values_are_refused() {
        assert!(parse(args("--to tv.local")).is_err());
        assert!(parse(args("--to 192.168.1.20 --codec av1")).is_err());
        assert!(parse(args("--to 192.168.1.20 --output")).is_err());
        assert!(parse(args("--to 192.168.1.20 --loud")).is_err());
    }
}
