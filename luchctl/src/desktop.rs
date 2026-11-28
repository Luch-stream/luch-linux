//! `luchctl --pick ...`, what a file manager's "Show on TV" runs: there is no terminal to ask or answer on, so which
//! receiver, the files when none were given, a pairing code and what went wrong go through the desktop's own dialog
//! tool (kdialog on KDE, zenity elsewhere), which every such desktop already has: no new dependency.

use std::sync::atomic::{AtomicBool, Ordering};

static ON: AtomicBool = AtomicBool::new(false);

pub fn enable() {
    ON.store(true, Ordering::Relaxed);
}

pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Zenity,
    Kdialog,
}

impl Tool {
    fn bin(self) -> &'static str {
        match self {
            Tool::Zenity => "zenity",
            Tool::Kdialog => "kdialog",
        }
    }
}

/// kdialog on KDE when it is there, zenity anywhere else, and whichever there is otherwise.
pub fn choose(desktop: &str, has: impl Fn(&str) -> bool) -> Option<Tool> {
    let kde = desktop.split(':').any(|d| d.eq_ignore_ascii_case("kde"));
    let order = if kde {
        [Tool::Kdialog, Tool::Zenity]
    } else {
        [Tool::Zenity, Tool::Kdialog]
    };
    order.into_iter().find(|t| has(t.bin()))
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

fn tool() -> Option<Tool> {
    choose(
        &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        on_path,
    )
}

/// The list of receivers; the answer is the id.
pub fn pick_args(t: Tool, all: &[(String, String)]) -> Vec<String> {
    let mut a: Vec<String> = match t {
        Tool::Zenity => [
            "--list",
            "--title=Luch",
            "--text=Which TV?",
            "--column=id",
            "--column=TV",
            "--hide-column=1",
            "--print-column=1",
            "--hide-header",
        ]
        .map(String::from)
        .to_vec(),
        Tool::Kdialog => ["--title", "Luch", "--menu", "Which TV?"]
            .map(String::from)
            .to_vec(),
    };
    for (id, name) in all {
        a.push(id.clone());
        a.push(name.clone());
    }
    a
}

pub fn code_args(t: Tool, name: &str) -> Vec<String> {
    let text = format!("Type the code shown on {name}");
    match t {
        Tool::Zenity => vec![
            "--entry".into(),
            "--title=Pair with Luch".into(),
            format!("--text={text}"),
        ],
        Tool::Kdialog => vec![
            "--title".into(),
            "Pair with Luch".into(),
            "--inputbox".into(),
            text,
        ],
    }
}

pub fn error_args(t: Tool, msg: &str) -> Vec<String> {
    match t {
        Tool::Zenity => vec![
            "--error".into(),
            "--title=Luch".into(),
            "--no-markup".into(),
            format!("--text={msg}"),
        ],
        Tool::Kdialog => vec![
            "--title".into(),
            "Luch".into(),
            "--error".into(),
            msg.into(),
        ],
    }
}

/// Several files, one per line in the answer.
pub fn files_args(t: Tool, title: &str, home: &str) -> Vec<String> {
    match t {
        Tool::Zenity => vec![
            "--file-selection".into(),
            "--multiple".into(),
            "--separator=\n".into(),
            format!("--title={title}"),
        ],
        Tool::Kdialog => vec![
            "--title".into(),
            title.into(),
            "--multiple".into(),
            "--separate-output".into(),
            "--getopenfilename".into(),
            home.into(),
        ],
    }
}

/// What the dialog printed, or None when it was closed or cancelled.
fn ask(t: Tool, args: &[String]) -> Option<String> {
    let out = std::process::Command::new(t.bin())
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout)
        .trim_end_matches('\n')
        .to_string();
    (!s.is_empty()).then_some(s)
}

/// The person closed the dialog: nothing went wrong, so nothing is said.
fn cancelled() -> ! {
    std::process::exit(1)
}

/// Which receiver; None when there is no dialog tool to ask with.
pub fn pick(all: &[(String, String)]) -> Option<String> {
    let t = tool()?;
    match ask(t, &pick_args(t, all)) {
        // zenity of GTK 3 prints the column once per row selected, with a | between.
        Some(id) => Some(id.split('|').next().unwrap_or(&id).to_string()),
        None => cancelled(),
    }
}

pub fn code(name: &str) -> Option<String> {
    let t = tool()?;
    match ask(t, &code_args(t, name)) {
        Some(c) => Some(c.trim().to_string()),
        None => cancelled(),
    }
}

/// The files to show when the menu gave none (the launcher's icon was clicked).
pub fn files(title: &str) -> Option<Vec<String>> {
    let t = tool()?;
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    match ask(t, &files_args(t, title, &home)) {
        Some(s) => Some(s.lines().map(str::to_string).collect()),
        None => cancelled(),
    }
}

/// A refusal shown where the person is looking; a notification when there is no dialog tool.
pub fn error(msg: &str) {
    let shown = tool().is_some_and(|t| {
        std::process::Command::new(t.bin())
            .args(error_args(t, msg))
            .status()
            .is_ok()
    });
    if !shown {
        let _ = std::process::Command::new("notify-send")
            .args(["--app-name=Luch", "Luch", msg])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<(String, String)> {
        vec![
            ("a1".into(), "Living room TV".into()),
            ("b2".into(), "Kitchen".into()),
        ]
    }

    #[test]
    fn kdialog_on_kde_zenity_elsewhere() {
        let both = |_: &str| true;
        assert_eq!(choose("KDE", both), Some(Tool::Kdialog));
        assert_eq!(choose("ubuntu:GNOME", both), Some(Tool::Zenity));
        assert_eq!(choose("", both), Some(Tool::Zenity));
        assert_eq!(choose("KDE", |b| b == "zenity"), Some(Tool::Zenity));
        assert_eq!(choose("GNOME", |b| b == "kdialog"), Some(Tool::Kdialog));
        assert_eq!(choose("GNOME", |_| false), None);
    }

    #[test]
    fn the_list_answers_with_the_id() {
        let z = pick_args(Tool::Zenity, &all());
        assert!(z.contains(&"--print-column=1".to_string()));
        assert_eq!(&z[z.len() - 4..], ["a1", "Living room TV", "b2", "Kitchen"]);
        let k = pick_args(Tool::Kdialog, &all());
        assert_eq!(
            k,
            [
                "--title",
                "Luch",
                "--menu",
                "Which TV?",
                "a1",
                "Living room TV",
                "b2",
                "Kitchen"
            ]
        );
    }

    #[test]
    fn an_error_is_plain_text() {
        let z = error_args(Tool::Zenity, "no receiver '<tv>' & co");
        assert!(z.contains(&"--no-markup".to_string()));
        assert_eq!(z.last().unwrap(), "--text=no receiver '<tv>' & co");
        assert_eq!(error_args(Tool::Kdialog, "x").last().unwrap(), "x");
    }

    #[test]
    fn the_code_and_the_files() {
        assert_eq!(
            code_args(Tool::Zenity, "Kitchen").last().unwrap(),
            "--text=Type the code shown on Kitchen"
        );
        assert_eq!(
            files_args(Tool::Kdialog, "Show on TV", "/home/u")
                .last()
                .unwrap(),
            "/home/u"
        );
        assert!(files_args(Tool::Zenity, "Show on TV", "/home/u").contains(&"--multiple".into()));
    }
}
