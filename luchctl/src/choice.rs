//! Which receiver a command means when it names none and there are several: a numbered list on the terminal, a
//! number or the start of a name to pick, Enter for the one picked last time.

use std::io::{BufRead, IsTerminal, Write};

/// The list as printed, the last one picked marked with a star.
pub fn menu(all: &[(String, String)], last: Option<&str>) -> String {
    all.iter()
        .enumerate()
        .map(|(i, (id, name))| {
            let star = if Some(id.as_str()) == last { "  *" } else { "" };
            format!("  {}) {name}{star}", i + 1)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The id an answer picks: a number, the start of one name only (an exact name wins), or nothing for the last one.
pub fn pick(answer: &str, all: &[(String, String)], last: Option<&str>) -> Option<String> {
    let a = answer.trim();
    if a.is_empty() {
        return last
            .filter(|l| all.iter().any(|(id, _)| id == l))
            .map(str::to_string);
    }
    if let Ok(n) = a.parse::<usize>() {
        return all.get(n.checked_sub(1)?).map(|(id, _)| id.clone());
    }
    let a = a.to_lowercase();
    if let Some((id, _)) = all.iter().find(|(_, n)| n.to_lowercase() == a) {
        return Some(id.clone());
    }
    match all
        .iter()
        .filter(|(_, n)| n.to_lowercase().starts_with(&a))
        .collect::<Vec<_>>()[..]
    {
        [(id, _)] => Some(id.clone()),
        _ => None,
    }
}

fn last_file() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))?;
    Some(base.join("luch").join("last-receiver"))
}

/// Asks on the terminal; None when there is none to ask on (a script gets the error instead) or no answer picks one.
pub fn ask(all: &[(String, String)]) -> Option<String> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return None;
    }
    let file = last_file();
    let last = file
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| s.trim().to_string());
    let last = last.as_deref();
    println!("Which receiver?\n{}", menu(all, last));
    let default = last.and_then(|l| all.iter().find(|(id, _)| id == l));
    let mut lines = std::io::stdin().lock().lines();
    loop {
        match default {
            Some((_, name)) => print!("number or name [{name}]: "),
            None => print!("number or name: "),
        }
        let _ = std::io::stdout().flush();
        let line = lines.next()?.ok()?;
        if let Some(id) = pick(&line, all, last) {
            if let Some(f) = &file {
                let _ = std::fs::create_dir_all(f.parent()?);
                let _ = std::fs::write(f, format!("{id}\n"));
            }
            return Some(id);
        }
        println!("no such receiver");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<(String, String)> {
        [
            ("a", "Living room TV"),
            ("b", "Living room TV (dev)"),
            ("c", "Bedroom projector"),
        ]
        .iter()
        .map(|(i, n)| (i.to_string(), n.to_string()))
        .collect()
    }

    #[test]
    fn a_number_a_name_or_enter_picks() {
        assert_eq!(pick("2", &all(), None).as_deref(), Some("b"));
        assert_eq!(pick(" bed ", &all(), None).as_deref(), Some("c"));
        assert_eq!(pick("living room tv", &all(), None).as_deref(), Some("a"));
        assert_eq!(pick("", &all(), Some("c")).as_deref(), Some("c"));
    }

    #[test]
    fn what_picks_nothing() {
        assert_eq!(pick("4", &all(), None), None);
        assert_eq!(pick("0", &all(), None), None);
        assert_eq!(pick("Living", &all(), None), None, "two names start so");
        assert_eq!(pick("", &all(), None), None);
        assert_eq!(
            pick("", &all(), Some("gone")),
            None,
            "the last one is not around"
        );
    }

    #[test]
    fn the_menu_marks_the_last_one() {
        assert_eq!(
            menu(&all(), Some("b")),
            "  1) Living room TV\n  2) Living room TV (dev)  *\n  3) Bedroom projector"
        );
    }
}
