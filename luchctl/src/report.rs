//! `luchctl report [--keep-names] [what happened]`: luchd's Report, the archive for a tester to hand over. `report
//! <n>` with a number stays the receivers' crash report n.

use std::io::{BufRead, IsTerminal, Write};

/// Whether `report ...` asks for a problem report rather than crash report n.
pub fn is_problem(rest: &[&str]) -> bool {
    rest.first().is_none_or(|w| w.parse::<usize>().is_err())
}

/// `--keep-names` and the words of what happened.
pub fn args(rest: &[&str]) -> Result<(bool, String), String> {
    let mut keep = false;
    let mut words = Vec::new();
    for w in rest {
        match *w {
            "--keep-names" => keep = true,
            f if f.starts_with("--") => {
                return Err(format!("report [--keep-names] [what happened], not {f}"))
            }
            w => words.push(w),
        }
    }
    Ok((keep, words.join(" ")))
}

/// What happened, asked on a terminal; nothing otherwise (a script gives it as words).
pub fn ask() -> String {
    if !std::io::stdin().is_terminal() {
        return String::new();
    }
    print!("What happened? (Enter to skip) ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().to_string()
}

/// Report's answer for a person: the file first (a script takes the first line), then what to do with it.
pub fn lines(json: &str, keep: bool) -> String {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let mut out = format!("{}\n", v["path"].as_str().unwrap_or(""));
    for n in v["notes"].as_array().into_iter().flatten() {
        out += &format!("  {}\n", n.as_str().unwrap_or(""));
    }
    if !keep {
        out += "addresses and names are replaced in it (--keep-names keeps them)\n";
    }
    out += "for a chat:\n";
    for l in v["description"].as_str().unwrap_or("").lines() {
        out += &format!("  {l}\n");
    }
    out += &format!(
        "an issue, filled in (attach the file to it):\n  {}\n",
        v["issue"].as_str().unwrap_or("")
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_is_a_crash_report_anything_else_a_problem() {
        assert!(is_problem(&[]));
        assert!(is_problem(&["--keep-names"]));
        assert!(is_problem(&["HEVC", "stutters"]));
        assert!(!is_problem(&["2"]));
        assert!(!is_problem(&["2", "open"]));
    }

    #[test]
    fn the_words_are_what_happened() {
        assert_eq!(
            args(&["HEVC", "--keep-names", "stutters"]),
            Ok((true, "HEVC stutters".into()))
        );
        assert_eq!(args(&[]), Ok((false, String::new())));
        assert!(args(&["--keep"]).is_err());
    }

    #[test]
    fn the_file_comes_first() {
        let json = serde_json::json!({"path": "/tmp/Downloads/Luch-report-2026-01-02-0304.zip",
            "description": "What happened: x\nReport: Luch-report-2026-01-02-0304.zip",
            "issue": "https://example.org/new?title=x", "notes": ["Receiver 2: no log, it is off or away"]});
        assert_eq!(
            lines(&json.to_string(), false),
            "/tmp/Downloads/Luch-report-2026-01-02-0304.zip\n  Receiver 2: no log, it is off or away\n\
             addresses and names are replaced in it (--keep-names keeps them)\nfor a chat:\n  What happened: x\n  \
             Report: Luch-report-2026-01-02-0304.zip\nan issue, filled in (attach the file to it):\n  \
             https://example.org/new?title=x\n"
        );
    }
}
