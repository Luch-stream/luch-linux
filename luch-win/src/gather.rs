//! Explorer starts a context-menu command once per selected file, so ten photos are ten processes. The first one to
//! bind a loopback port gathers the others' files and casts them all as one slideshow; the others hand their file
//! over and end. (Send To passes all files to one process and needs none of this.)

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// Loopback only; one past the ports Luch uses on the LAN (47800-47810).
pub const PORT: u16 = 47819;

const HI: &str = "luch-win gathers\n";

/// Explorer starts the processes of one selection within a few hundred ms of each other.
pub const QUIET: Duration = Duration::from_millis(700);

/// All files of this selection, sorted as a folder shows them, when this process is the one to cast them; None
/// when another one took them.
pub fn gather(port: u16, mine: Vec<String>, quiet: Duration) -> Option<Vec<String>> {
    let at = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let l = match TcpListener::bind(at) {
        Ok(l) => l,
        Err(_) => {
            // Another process gathers (it says so first): hand over, or cast alone if it is gone or not ours.
            if let Ok(mut s) = TcpStream::connect_timeout(&at.into(), Duration::from_millis(300)) {
                let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
                let mut hi = String::new();
                let _ = BufReader::new(&s).read_line(&mut hi);
                let lines: String = mine.iter().map(|f| format!("{f}\n")).collect();
                if hi == HI && s.write_all(lines.as_bytes()).is_ok() {
                    return None;
                }
            }
            return Some(mine);
        }
    };
    let _ = l.set_nonblocking(true);
    let mut all = mine;
    let started = Instant::now();
    let mut last = started;
    // Some seconds at most, however many keep coming.
    while last.elapsed() < quiet && started.elapsed() < quiet * 8 {
        match l.accept() {
            Ok((mut s, _)) => {
                let _ = s.set_nonblocking(false);
                let _ = s.write_all(HI.as_bytes());
                let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
                all.extend(
                    BufReader::new(s)
                        .lines()
                        .map_while(Result::ok)
                        .filter(|l| !l.is_empty()),
                );
                last = Instant::now();
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    all.sort_by_key(|f| f.to_lowercase());
    all.dedup();
    Some(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn one_process_casts_the_whole_selection() {
        let port = free_port();
        let quiet = Duration::from_millis(300);
        let first = std::thread::spawn(move || gather(port, vec!["C:\\p\\c.jpg".into()], quiet));
        std::thread::sleep(Duration::from_millis(50));
        let others: Vec<_> = ["C:\\p\\A.jpg", "C:\\p\\b.jpg"]
            .into_iter()
            .map(|f| std::thread::spawn(move || gather(port, vec![f.into()], quiet)))
            .collect();
        for o in others {
            assert_eq!(o.join().unwrap(), None, "handed over");
        }
        assert_eq!(
            first.join().unwrap().unwrap(),
            ["C:\\p\\A.jpg", "C:\\p\\b.jpg", "C:\\p\\c.jpg"]
        );
    }

    #[test]
    fn alone_it_casts_its_own() {
        let port = free_port();
        assert_eq!(
            gather(port, vec!["a.mp4".into()], Duration::from_millis(50)),
            Some(vec!["a.mp4".to_string()])
        );
    }
}
