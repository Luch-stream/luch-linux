//! luch-win: mirror this Windows PC's screen to a Luch receiver. See docs/windows.md.

fn main() {
    let opts = match luch_win::cli::parse(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    #[cfg(windows)]
    if let Err(e) = luch_win::win::run(&opts) {
        eprintln!("luch-win: {e}");
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    {
        let _ = opts;
        eprintln!("luch-win captures a Windows screen; on Linux use luchd");
        std::process::exit(2);
    }
}
