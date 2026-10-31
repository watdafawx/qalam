//! Per-user install (no admin): %LOCALAPPDATA%\Programs\qalam, Start Menu shortcut,
//! "Start with Windows", and an entry in Settings > Apps for uninstalling. Or portable mode.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use qalam::config;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TDCBF_CANCEL_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_USE_COMMAND_LINKS,
    TaskDialogIndirect,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_COMMAND};
use windows::core::{BOOL, HSTRING, Interface, PCWSTR, w};

const UNINSTALL_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\qalam");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Programs").join("qalam")
}

pub fn installed_exe() -> PathBuf {
    install_dir().join("qalam.exe")
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

/// Running the installed copy?
pub fn is_installed_copy() -> bool {
    std::env::current_exe().is_ok_and(|p| same_path(&p, &installed_exe()))
}

fn shortcut_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Microsoft\\Windows\\Start Menu\\Programs\\qalam.lnk")
}

fn reg_set_str(key: PCWSTR, name: PCWSTR, value: &str) -> bool {
    let data: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, key, name, REG_SZ.0, Some(data.as_ptr().cast()), (data.len() * 2) as u32).is_ok()
    }
}

fn reg_get_str(key: PCWSTR, name: PCWSTR) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key, name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut len)).ok().ok()?;
    }
    let n = (len as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..n]))
}

/// "Start with Windows": a value under HKCU\...\Run pointing at an exe.
pub mod autostart {
    use super::*;

    const KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    const NAME: PCWSTR = w!("qalam");

    fn command(exe: &Path) -> String {
        format!("\"{}\"", exe.display())
    }

    /// On only if the entry points at `exe` (an entry for another copy counts as off).
    pub fn enabled_for(exe: &Path) -> bool {
        reg_get_str(KEY, NAME).is_some_and(|v| v.eq_ignore_ascii_case(&command(exe)))
    }

    pub fn enabled() -> bool {
        std::env::current_exe().is_ok_and(|e| enabled_for(&e))
    }

    pub fn set_for(exe: &Path, on: bool) -> bool {
        if on {
            reg_set_str(KEY, NAME, &command(exe))
        } else {
            unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, KEY, NAME).is_ok() }
        }
    }

    /// Remove the Run entry left by builds named "typeahead".
    pub fn remove_legacy() {
        unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, KEY, w!("typeahead"));
        }
    }

    pub fn set(on: bool) -> bool {
        std::env::current_exe().is_ok_and(|e| set_for(&e, on))
    }
}

fn create_shortcut(target: &Path, lnk: &Path) -> windows::core::Result<()> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(target.as_os_str()))?;
        link.SetDescription(w!("Word suggestions, autocorrect and text replacement"))?;
        if let Some(dir) = target.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true)
    }
}

/// Copy this exe into place and register it. Returns the installed exe.
pub fn install() -> Result<PathBuf, String> {
    let src = std::env::current_exe().map_err(|e| e.to_string())?;
    let dst = installed_exe();
    std::fs::create_dir_all(install_dir()).map_err(|e| format!("Can't create {}: {e}", install_dir().display()))?;
    if !same_path(&src, &dst) {
        std::fs::copy(&src, &dst).map_err(|e| format!("Can't copy to {}: {e}", dst.display()))?;
    }
    let _ = create_shortcut(&dst, &shortcut_path());
    autostart::set_for(&dst, true);

    let exe = dst.display().to_string();
    reg_set_str(UNINSTALL_KEY, w!("DisplayName"), "qalam");
    reg_set_str(UNINSTALL_KEY, w!("DisplayVersion"), env!("CARGO_PKG_VERSION"));
    reg_set_str(UNINSTALL_KEY, w!("Publisher"), "qalam");
    reg_set_str(UNINSTALL_KEY, w!("DisplayIcon"), &exe);
    reg_set_str(UNINSTALL_KEY, w!("InstallLocation"), &install_dir().display().to_string());
    reg_set_str(UNINSTALL_KEY, w!("UninstallString"), &format!("\"{exe}\" --uninstall"));
    let one = 1u32.to_le_bytes();
    for name in [w!("NoModify"), w!("NoRepair")] {
        unsafe {
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, UNINSTALL_KEY, name, REG_DWORD.0, Some(one.as_ptr().cast()), 4);
        }
    }
    Ok(dst)
}

/// Start the installed copy (it waits for this process to exit before taking over).
pub fn launch_installed(exe: &Path) {
    let _ = std::process::Command::new(exe).arg("--after-install").spawn();
}

/// Ask a running instance to quit and wait a bit for it.
fn quit_running_instance() {
    unsafe {
        if let Ok(h) = FindWindowW(w!("qalam_tray"), None) {
            let _ = PostMessageW(Some(h), WM_COMMAND, WPARAM(crate::MENU_QUIT), LPARAM(0));
            for _ in 0..30 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if FindWindowW(w!("qalam_tray"), None).is_err() {
                    break;
                }
            }
        }
    }
}

/// Settings > Apps > qalam > Uninstall runs `qalam.exe --uninstall`.
pub fn uninstall() {
    let mut delete_data: BOOL = BOOL(0);
    let choice = task_dialog(
        "Uninstall qalam?",
        "Removes the app, its Start Menu shortcut and Start with Windows.",
        &[(100, "Uninstall")],
        Some(("Also delete my replacements, secrets and learned words", &mut delete_data)),
    );
    if choice != 100 {
        return;
    }
    quit_running_instance();
    let installed = installed_exe();
    if autostart::enabled_for(&installed) {
        autostart::set_for(&installed, false);
    }
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, UNINSTALL_KEY);
    }
    let _ = std::fs::remove_file(shortcut_path());
    if delete_data.as_bool() {
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default().join("qalam");
        let _ = std::fs::remove_dir_all(appdata);
    }
    // We're running from the folder being deleted, so let cmd remove it once we've exited.
    let dir = install_dir();
    let _ = std::process::Command::new("cmd.exe")
        .raw_arg(format!("/c ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display()))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

pub enum FirstRun {
    Install,
    Portable,
    JustRun,
    Quit,
}

/// Shown when running a copy that's neither installed nor portable.
pub fn first_run_prompt() -> FirstRun {
    let portable_dir = config::exe_dir().map(|d| d.join(config::PORTABLE_DIR)).unwrap_or_default();
    let install_text = format!(
        "Install for this user\nCopies to {} with a Start Menu shortcut, starts with Windows, uninstall from Settings > Apps.",
        install_dir().display()
    );
    let portable_text = format!("Run portable from this folder\nSettings are kept in {} next to the exe.", portable_dir.display());
    match task_dialog(
        "How do you want to run qalam?",
        "Word suggestions, autocorrect and text replacement for every app.",
        &[(100, &install_text), (101, &portable_text), (102, "Just run it this time")],
        None,
    ) {
        100 => FirstRun::Install,
        101 => {
            if std::fs::create_dir_all(&portable_dir).is_ok() { FirstRun::Portable } else { FirstRun::JustRun }
        }
        102 => FirstRun::JustRun,
        _ => FirstRun::Quit,
    }
}

/// Task dialog with command-link buttons. Returns the clicked button id (or IDCANCEL = 2).
fn task_dialog(title: &str, body: &str, buttons: &[(i32, &str)], check: Option<(&str, &mut BOOL)>) -> i32 {
    let title_w = HSTRING::from(title);
    let body_w = HSTRING::from(body);
    let labels: Vec<HSTRING> = buttons.iter().map(|(_, t)| HSTRING::from(*t)).collect();
    let btns: Vec<TASKDIALOG_BUTTON> = buttons
        .iter()
        .zip(&labels)
        .map(|((id, _), l)| TASKDIALOG_BUTTON { nButtonID: *id, pszButtonText: PCWSTR(l.as_ptr()) })
        .collect();
    let (check_text, check_out) = match check {
        Some((t, out)) => (Some(HSTRING::from(t)), Some(out)),
        None => (None, None),
    };
    let mut cfg = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: HWND::default(),
        dwFlags: TDF_USE_COMMAND_LINKS | TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: w!("qalam"),
        pszMainInstruction: PCWSTR(title_w.as_ptr()),
        pszContent: PCWSTR(body_w.as_ptr()),
        cButtons: btns.len() as u32,
        pButtons: btns.as_ptr(),
        ..Default::default()
    };
    if let Some(t) = &check_text {
        cfg.pszVerificationText = PCWSTR(t.as_ptr());
    }
    let mut pressed = 0i32;
    let mut checked = BOOL(0);
    unsafe {
        if TaskDialogIndirect(&cfg, Some(&mut pressed), None, Some(&mut checked)).is_err() {
            return 2;
        }
    }
    if let Some(out) = check_out {
        *out = checked;
    }
    pressed
}
