//! `luch-latency analyze VIDEO` — the same as `luchctl latency analyze`, without D-Bus, on any computer.
//! `luch-latency synth OUT.mp4 [--latency-ms N]` — a synthetic video of a known latency, to try the analyser.
//! `luch-latency scan IMAGE` — the patterns in one picture (a screenshot of the TV, a photo).

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        Some("analyze") => luch_latency::cli::analyze(&args[1..]),
        Some("synth") => luch_latency::cli::synth(&args[1..]),
        Some("scan") if args.len() == 2 => luch_latency::cli::scan(Path::new(&args[1])),
        _ => Err(format!(
            "usage:\n  luch-latency analyze VIDEO [--out PREFIX] [--fps N] [--readout-ms MS]\n  \
             luch-latency synth OUT.mp4 [--latency-ms MS] [--seconds S]\n  luch-latency scan IMAGE\n\n{}",
            luch_latency::cli::ANALYZE_HELP
        )),
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
