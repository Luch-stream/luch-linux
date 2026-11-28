//! `install-shell` and `uninstall-shell`: the keys and shortcuts `crate::shell` describes, written for the current
//! user, then Explorer told to read its menus again. Also the two things a cast started from Explorer needs: no
//! console window, and a refusal in a message box rather than on a console nobody sees.

use std::path::PathBuf;

use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::Shell::{
    IShellLinkW, SHChangeNotify, ShellLink, SHCNE_ASSOCCHANGED, SHCNF_IDLIST,
};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

use crate::shell::{self, Key, Receiver};

fn appdata() -> Result<PathBuf, String> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "APPDATA is not set".into())
}

fn list_file() -> Result<PathBuf, String> {
    Ok(appdata()?.join("Luch").join("receivers"))
}

fn send_to_dir() -> Result<PathBuf, String> {
    Ok(appdata()?.join(r"Microsoft\Windows\SendTo"))
}

pub fn receivers() -> Vec<Receiver> {
    list_file()
        .ok()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|t| shell::parse_list(&t))
        .unwrap_or_default()
}

fn set(key: &Key) -> Result<(), String> {
    let mut h = HKEY::default();
    // SAFETY: a key under HKCU, closed below.
    let e = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key.path.as_str()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut h,
            None,
        )
    };
    e.ok().map_err(|e| format!(r"HKCU\{}: {e}", key.path))?;
    let mut result = Ok(());
    for (name, value) in &key.values {
        let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        let bytes: Vec<u8> = wide.iter().flat_map(|c| c.to_le_bytes()).collect();
        let name = HSTRING::from(*name);
        // SAFETY: an open key; REG_SZ data is the string with its terminating NUL.
        let e = unsafe { RegSetValueExW(h, &name, None, REG_SZ, Some(&bytes)) };
        if let Err(e) = e.ok() {
            result = Err(format!(r"HKCU\{} {name}: {e}", key.path));
            break;
        }
    }
    // SAFETY: opened above.
    let _ = unsafe { RegCloseKey(h) };
    result
}

fn remove_menus() {
    for k in shell::MENU_KEYS {
        // SAFETY: a key under HKCU; a missing one is fine.
        let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(k)) };
    }
}

fn remove_links() {
    let Ok(dir) = send_to_dir() else { return };
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.file_name()
            .to_string_lossy()
            .ends_with(shell::LINK_SUFFIX)
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn link(exe: &str, args: &str, at: &std::path::Path) -> Result<(), String> {
    // SAFETY: COM on this thread for the shell link object, released when it drops.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let l: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        l.SetPath(&HSTRING::from(exe)).map_err(|e| e.to_string())?;
        l.SetArguments(&HSTRING::from(args))
            .map_err(|e| e.to_string())?;
        l.SetIconLocation(&HSTRING::from(exe), 0)
            .map_err(|e| e.to_string())?;
        l.SetDescription(&HSTRING::from("Show on TV with Luch"))
            .map_err(|e| e.to_string())?;
        let f: IPersistFile = l.cast().map_err(|e| e.to_string())?;
        f.Save(&HSTRING::from(at.as_os_str()), true)
            .map_err(|e| format!("{}: {e}", at.display()))
    }
}

/// Explorer reads the menus again (else only after a sign-in).
fn tell_explorer() {
    // SAFETY: no items with this event.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

/// Adds `r` (or, with none, writes again what is kept, as after luch-win.exe was moved) and writes the menus.
pub fn install(r: Option<Receiver>) -> Result<Vec<Receiver>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().into_owned();
    let mut list = receivers();
    if let Some(r) = r {
        list = shell::add(&list, r);
        let f = list_file()?;
        std::fs::create_dir_all(f.parent().unwrap_or(&f)).map_err(|e| e.to_string())?;
        std::fs::write(&f, shell::format_list(&list))
            .map_err(|e| format!("{}: {e}", f.display()))?;
    }
    if list.is_empty() {
        return Err(
            "no receiver yet: luch-win install-shell --to <TV address> --name <name>".into(),
        );
    }
    remove_menus();
    remove_links();
    for k in shell::menu_keys(&exe, &list) {
        set(&k)?;
    }
    let dir = send_to_dir()?;
    for (name, args) in shell::send_to(&list) {
        link(&exe, &args, &dir.join(name))?;
    }
    tell_explorer();
    Ok(list)
}

/// Every menu, shortcut and kept receiver gone.
pub fn uninstall() -> Result<(), String> {
    remove_menus();
    remove_links();
    if let Ok(f) = list_file() {
        let _ = std::fs::remove_file(f);
    }
    tell_explorer();
    Ok(())
}

/// Started by Explorer: the console window it opened for this program goes away at once.
pub fn hide_console() {
    // SAFETY: detaches this process from its console; nothing is written to it afterwards.
    let _ = unsafe { windows::Win32::System::Console::FreeConsole() };
}

pub fn message(text: &str) {
    // SAFETY: a modal box with no owner window.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("Luch"),
            MB_OK | MB_ICONERROR,
        );
    }
}
