//! Per-user file-type registration (no admin rights needed).
//!
//! Windows 10/11 protects the actual "default app" choice behind a hash, so an
//! app can only *register* as a candidate; the user then picks it once in
//! Settings > Default apps, which we open for them.

use std::io;
use std::path::{Path, PathBuf};

use winreg::enums::*;
use winreg::RegKey;

use crate::icon;
use crate::loader::EXTENSIONS;

const PROG_ID: &str = "WinPicView.Image";
const APP_NAME: &str = "WinPicView";
const EXE_NAME: &str = "winpicview.exe";

fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Programs").join(APP_NAME)
}

/// Copies the running exe to a stable per-user location and registers it.
pub fn install() -> io::Result<PathBuf> {
    let dir = install_dir();
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(EXE_NAME);
    let me = std::env::current_exe()?;
    if me != target {
        std::fs::copy(&me, &target)?;
    }
    register(&target)?;
    Ok(target)
}

pub fn register(exe: &Path) -> io::Result<()> {
    let exe_s = exe.to_string_lossy().to_string();
    let ico = exe.with_file_name("winpicview.ico");
    std::fs::write(&ico, icon::icon_ico())?;
    let ico_s = ico.to_string_lossy().to_string();
    let open_cmd = format!("\"{exe_s}\" \"%1\"");

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu.create_subkey("Software\\Classes")?.0;

    // ProgID used by every extension.
    let prog = classes.create_subkey(PROG_ID)?.0;
    prog.set_value("", &"Image")?;
    prog.create_subkey("DefaultIcon")?.0.set_value("", &ico_s)?;
    prog.create_subkey("shell\\open\\command")?.0.set_value("", &open_cmd)?;

    // "Open with" entry for the exe itself.
    let app = classes.create_subkey(format!("Applications\\{EXE_NAME}"))?.0;
    app.set_value("FriendlyAppName", &APP_NAME)?;
    app.create_subkey("shell\\open\\command")?.0.set_value("", &open_cmd)?;
    let supported = app.create_subkey("SupportedTypes")?.0;

    // Capabilities (makes the app show up in Settings > Default apps).
    let cap = hkcu.create_subkey("Software\\WinPicView\\Capabilities")?.0;
    cap.set_value("ApplicationName", &APP_NAME)?;
    cap.set_value("ApplicationDescription", &"Fast, minimal, offline photo viewer")?;
    let assoc = cap.create_subkey("FileAssociations")?.0;

    for ext in EXTENSIONS {
        let dot = format!(".{ext}");
        supported.set_value(&dot, &"")?;
        assoc.set_value(&dot, &PROG_ID)?;
        let k = classes.create_subkey(format!("{dot}\\OpenWithProgids"))?.0;
        k.set_value(PROG_ID, &"")?;
    }

    hkcu.create_subkey("Software\\RegisteredApplications")?
        .0
        .set_value(APP_NAME, &"Software\\WinPicView\\Capabilities")?;

    notify_assoc_changed();
    Ok(())
}

pub fn unregister() -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu.open_subkey_with_flags("Software\\Classes", KEY_ALL_ACCESS)?;
    for ext in EXTENSIONS {
        if let Ok(k) = classes.open_subkey_with_flags(format!(".{ext}\\OpenWithProgids"), KEY_ALL_ACCESS) {
            let _ = k.delete_value(PROG_ID);
        }
    }
    let _ = classes.delete_subkey_all(PROG_ID);
    let _ = classes.delete_subkey_all(format!("Applications\\{EXE_NAME}"));
    let _ = hkcu.delete_subkey_all("Software\\WinPicView");
    if let Ok(k) = hkcu.open_subkey_with_flags("Software\\RegisteredApplications", KEY_ALL_ACCESS) {
        let _ = k.delete_value(APP_NAME);
    }
    notify_assoc_changed();
    Ok(())
}

fn notify_assoc_changed() {
    use windows_sys::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, std::ptr::null(), std::ptr::null()) };
}

/// Opens Settings > Default apps on this app's page.
pub fn open_default_apps_settings() {
    let _ = std::process::Command::new("explorer")
        .arg(format!("ms-settings:defaultapps?registeredAppUser={APP_NAME}"))
        .spawn();
}
