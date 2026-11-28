//! "Show on TV with Luch" in Explorer: what `install-shell` writes, as data, so it is tested off Windows too
//! (`win::shell` writes it). Only the current user's keys (HKCU) and Send To folder: no administrator needed.
//!
//! - The context menu of photos, videos, sounds and PDFs: one receiver is one item, several are a submenu of them.
//!   On Windows 11 such items are under "Show more options" (the new menu takes only packaged apps' handlers).
//! - Send To: a shortcut per receiver, which gets every selected file in one go.
//!
//! There is no discovery on Windows yet, so the receivers are the ones given by address, kept in
//! %APPDATA%\Luch\receivers (a line per receiver: name, a tab, address:port).

use std::net::{Ipv4Addr, SocketAddrV4};

use luch_proto::CONTROL_PORT;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receiver {
    pub name: String,
    pub to: SocketAddrV4,
}

/// `IP` or `IP:PORT`, the control port by default.
pub fn address(s: &str) -> Result<SocketAddrV4, String> {
    s.parse::<SocketAddrV4>().or_else(|_| {
        s.parse::<Ipv4Addr>()
            .map(|ip| SocketAddrV4::new(ip, CONTROL_PORT))
            .map_err(|_| format!("not an IPv4 address: {s}"))
    })
}

pub fn parse_list(text: &str) -> Vec<Receiver> {
    text.lines()
        .filter_map(|l| {
            let (name, to) = l.split_once('\t')?;
            Some(Receiver {
                name: name.trim().to_string(),
                to: address(to.trim()).ok()?,
            })
        })
        .collect()
}

pub fn format_list(list: &[Receiver]) -> String {
    list.iter()
        .map(|r| format!("{}\t{}\n", r.name, r.to))
        .collect()
}

/// `r` in the list, in place of one with its name or address.
pub fn add(list: &[Receiver], r: Receiver) -> Vec<Receiver> {
    let mut out: Vec<_> = list
        .iter()
        .filter(|o| o.to != r.to && !o.name.eq_ignore_ascii_case(&r.name))
        .cloned()
        .collect();
    out.push(r);
    out
}

/// Where the menu goes, under HKCU: the kinds Windows itself groups files in, and PDFs, which it does not.
pub const MENU_KEYS: [&str; 4] = [
    r"Software\Classes\SystemFileAssociations\image\shell\LuchShow",
    r"Software\Classes\SystemFileAssociations\video\shell\LuchShow",
    r"Software\Classes\SystemFileAssociations\audio\shell\LuchShow",
    r"Software\Classes\SystemFileAssociations\.pdf\shell\LuchShow",
];

/// A registry key and its string values ("" is the key's default value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub path: String,
    pub values: Vec<(&'static str, String)>,
}

/// What a menu item runs: Explorer starts it once per selected file (`gather` joins them).
pub fn command(exe: &str, to: SocketAddrV4) -> String {
    format!("\"{exe}\" cast --shell --to {to} \"%1\"")
}

pub fn menu_keys(exe: &str, list: &[Receiver]) -> Vec<Key> {
    let icon = format!("{exe},0");
    let mut keys = Vec::new();
    for root in MENU_KEYS {
        match list {
            [] => {}
            [r] => {
                keys.push(Key {
                    path: root.into(),
                    values: vec![
                        ("MUIVerb", format!("Show on {}", r.name)),
                        ("Icon", icon.clone()),
                        // The item stays for a selection of more than 15 files.
                        ("MultiSelectModel", "Player".into()),
                    ],
                });
                keys.push(Key {
                    path: format!(r"{root}\command"),
                    values: vec![("", command(exe, r.to))],
                });
            }
            several => {
                keys.push(Key {
                    path: root.into(),
                    values: vec![
                        ("MUIVerb", "Show on TV".into()),
                        ("Icon", icon.clone()),
                        ("MultiSelectModel", "Player".into()),
                        // An empty SubCommands makes the item a submenu of the keys under its own `shell`.
                        ("SubCommands", String::new()),
                    ],
                });
                for (i, r) in several.iter().enumerate() {
                    let sub = format!(r"{root}\shell\tv{i:02}");
                    keys.push(Key {
                        path: sub.clone(),
                        values: vec![("MUIVerb", r.name.clone())],
                    });
                    keys.push(Key {
                        path: format!(r"{sub}\command"),
                        values: vec![("", command(exe, r.to))],
                    });
                }
            }
        }
    }
    keys
}

/// The end of every Send To shortcut this makes, so uninstall-shell finds them all and nothing else.
pub const LINK_SUFFIX: &str = " (Luch TV).lnk";

/// Send To shortcuts, which get every selected file in one go: file name and the arguments after luch-win.exe.
pub fn send_to(list: &[Receiver]) -> Vec<(String, String)> {
    list.iter()
        .map(|r| {
            let safe: String = r
                .name
                .chars()
                .map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c })
                .collect();
            (
                format!("{safe}{LINK_SUFFIX}"),
                format!("cast --shell --to {}", r.to),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(name: &str, to: &str) -> Receiver {
        Receiver {
            name: name.into(),
            to: address(to).unwrap(),
        }
    }

    #[test]
    fn the_list_round_trips_and_replaces() {
        let list = vec![r("Living room", "10.0.0.5"), r("Kitchen", "10.0.0.6:5000")];
        assert_eq!(parse_list(&format_list(&list)), list);
        assert_eq!(list[0].to.port(), CONTROL_PORT);
        assert_eq!(parse_list("junk\nTV\tnot an address\n"), []);
        let again = add(&list, r("living ROOM", "10.0.0.7"));
        assert_eq!(again.len(), 2, "the same name");
        assert_eq!(again[1].to, address("10.0.0.7").unwrap());
        assert_eq!(
            add(&list, r("Den", "10.0.0.6:5000")).len(),
            2,
            "the same address"
        );
        assert!(address("tv.local").is_err());
    }

    #[test]
    fn one_receiver_is_one_item() {
        let keys = menu_keys(r"C:\Luch\luch-win.exe", &[r("Living room", "10.0.0.5")]);
        assert_eq!(keys.len(), 8);
        assert_eq!(keys[0].path, MENU_KEYS[0]);
        assert_eq!(keys[0].values[0], ("MUIVerb", "Show on Living room".into()));
        assert_eq!(
            keys[1],
            Key {
                path: format!(r"{}\command", MENU_KEYS[0]),
                values: vec![(
                    "",
                    r#""C:\Luch\luch-win.exe" cast --shell --to 10.0.0.5:47800 "%1""#.into()
                )],
            }
        );
    }

    #[test]
    fn several_are_a_submenu() {
        let keys = menu_keys("luch-win.exe", &[r("A", "10.0.0.5"), r("B", "10.0.0.6")]);
        assert_eq!(keys.len(), 4 * 5);
        assert!(keys[0].values.contains(&("SubCommands", String::new())));
        assert_eq!(keys[3].path, format!(r"{}\shell\tv01", MENU_KEYS[0]));
        assert_eq!(keys[3].values, [("MUIVerb", "B".to_string())]);
        assert!(keys[4].values[0].1.contains("--to 10.0.0.6:47800"));
        assert!(menu_keys("x", &[]).is_empty());
    }

    #[test]
    fn send_to_gets_all_files() {
        let links = send_to(&[r("TV: big/one", "10.0.0.5")]);
        assert_eq!(
            links,
            [(
                "TV_ big_one (Luch TV).lnk".to_string(),
                "cast --shell --to 10.0.0.5:47800".to_string()
            )]
        );
    }
}
