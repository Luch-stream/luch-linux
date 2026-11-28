//! luch-win: mirror this Windows PC's screen to a Luch receiver, or show files on it. See docs/windows.md.

use std::path::PathBuf;

use luch_win::cast::{self, Step};
use luch_win::cli::{self, Command};
use luch_win::gather;

fn main() {
    let cmd = match cli::command(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let result = match cmd {
        Command::Cast(c) => show(c),
        #[cfg(windows)]
        Command::Mirror(o) => luch_win::win::run(&o),
        #[cfg(windows)]
        Command::InstallShell(r) => luch_win::win::shell::install(r).map(|list| {
            let names: Vec<_> = list.iter().map(|r| r.name.as_str()).collect();
            println!(
                "Show on TV is in Explorer's menu (Windows 11: Show more options) and in Send To, for: {}",
                names.join(", ")
            );
        }),
        #[cfg(windows)]
        Command::UninstallShell => luch_win::win::shell::uninstall(),
        #[cfg(not(windows))]
        _ => Err("only cast works off Windows; on Linux use luchd".into()),
    };
    if let Err(e) = result {
        eprintln!("luch-win: {e}");
        std::process::exit(1);
    }
}

/// The id kept for this PC and its name, as the mirroring session says them.
fn identity() -> (String, String) {
    #[cfg(windows)]
    return (luch_win::win::sender_id(), luch_win::win::host_name());
    #[cfg(not(windows))]
    (
        cast::token(),
        std::env::var("HOSTNAME").unwrap_or_else(|_| "luch-win".into()),
    )
}

/// `cast`: until the receiver is done with the files (this process serves them).
fn show(c: cli::Cast) -> Result<(), String> {
    #[cfg(windows)]
    if c.shell {
        luch_win::win::shell::hide_console();
    }
    let files = if c.shell {
        match gather::gather(gather::PORT, c.files, gather::QUIET) {
            Some(all) => all,
            None => return Ok(()),
        }
    } else {
        c.files
    };
    let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
    let (id, host) = identity();
    let shell = c.shell;
    let r = cast::run(c.to, c.from, &files, &id, &host, &mut |s| {
        if !shell {
            eprintln!(
                "{}",
                match s {
                    Step::WaitingForAllow => "allow this PC on the TV".to_string(),
                    Step::Showing(kind) => format!("on the TV ({kind}); Ctrl-C ends it"),
                }
            );
        }
    });
    #[cfg(windows)]
    if let (Err(e), true) = (&r, shell) {
        luch_win::win::shell::message(e);
    }
    r
}
