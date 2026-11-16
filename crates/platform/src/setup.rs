//! Installing and removing Mini for the current user (no administrator rights): the exe in
//! `%LOCALAPPDATA%\Programs\Drxwnify Mini`, shortcuts in the Start menu (and on the desktop when asked), an entry in
//! «Apps» for removal (HKCU …\Uninstall\DrxwnifyMini). The `drxwnify://` scheme is registered by Mini at its first start.
//!
//! `DRX_SETUP_ROOT=<folder>` (checks): everything goes under that folder and a test registry key instead.

use std::path::{Path, PathBuf};
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_YESNO, MESSAGEBOX_STYLE,
    MessageBoxW,
};
use windows::core::{HSTRING, Interface};

pub const APP_NAME: &str = "Drxwnify Mini";
const EXE: &str = "drxwnify-mini.exe";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\DrxwnifyMini";
const TEST_KEY: &str = r"Software\DrxwnifySetupTest\Uninstall\DrxwnifyMini";

/// Where things go.
#[derive(Debug, Clone)]
pub struct Layout {
    pub dir: PathBuf,
    pub exe: PathBuf,
    pub start_menu_link: PathBuf,
    pub desktop_link: PathBuf,
    pub uninstall_key: String,
}

fn known(id: &windows::core::GUID) -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    // SAFETY: the returned string is read once and freed with CoTaskMemFree, as the API requires.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

impl Layout {
    pub fn current() -> Option<Layout> {
        use windows::Win32::UI::Shell::{FOLDERID_Desktop, FOLDERID_LocalAppData, FOLDERID_Programs};
        if let Some(root) = std::env::var_os("DRX_SETUP_ROOT").map(PathBuf::from) {
            let dir = root.join("Programs").join(APP_NAME);
            return Some(Layout {
                exe: dir.join(EXE),
                dir,
                start_menu_link: root.join("StartMenu").join(format!("{APP_NAME}.lnk")),
                desktop_link: root.join("Desktop").join(format!("{APP_NAME}.lnk")),
                uninstall_key: TEST_KEY.into(),
            });
        }
        let dir = known(&FOLDERID_LocalAppData)?.join("Programs").join(APP_NAME);
        Some(Layout {
            exe: dir.join(EXE),
            dir,
            start_menu_link: known(&FOLDERID_Programs)?.join(format!("{APP_NAME}.lnk")),
            desktop_link: known(&FOLDERID_Desktop)?.join(format!("{APP_NAME}.lnk")),
            uninstall_key: UNINSTALL_KEY.into(),
        })
    }

    /// Mini is installed here (its uninstall entry exists).
    pub fn installed_version(&self) -> Option<String> {
        crate::windows_impl::reg_get(&self.uninstall_key, "DisplayVersion")
    }
}

/// A yes/no question in a system dialog.
pub fn ask(title: &str, text: &str) -> bool {
    // SAFETY: plain strings into a modal system dialog.
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), MB_YESNO | MB_ICONQUESTION) == IDYES }
}

/// A yes/no question about something that cannot be undone: «No» is the default button.
pub fn ask_careful(title: &str, text: &str) -> bool {
    // SAFETY: plain strings into a modal system dialog.
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) == IDYES
    }
}

/// A message with «OK».
pub fn tell(title: &str, text: &str) {
    let style: MESSAGEBOX_STYLE = MB_OK | MB_ICONINFORMATION;
    // SAFETY: plain strings into a modal system dialog.
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), style);
    }
}

/// A shortcut (.lnk) to `target`.
pub fn create_shortcut(link: &Path, target: &Path, description: &str) -> Result<(), String> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // SAFETY: COM objects used on this thread and released when they drop.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let t = HSTRING::from(target.as_os_str());
        sl.SetPath(&t).map_err(|e| e.to_string())?;
        sl.SetIconLocation(&t, 0).map_err(|e| e.to_string())?;
        sl.SetDescription(&HSTRING::from(description)).map_err(|e| e.to_string())?;
        if let Some(dir) = target.parent() {
            sl.SetWorkingDirectory(&HSTRING::from(dir.as_os_str())).map_err(|e| e.to_string())?;
        }
        let file: IPersistFile = sl.cast().map_err(|e| e.to_string())?;
        file.Save(&HSTRING::from(link.as_os_str()), true).map_err(|e| e.to_string())
    }
}

fn delete_key_tree(key: &str) {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteTreeW};
    // SAFETY: a key path under HKCU.
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key));
    }
}

/// Writes Mini's exe and its entries. A running Mini keeps working: its file is moved aside first (Windows lets a
/// running exe be renamed) and goes away at the next install or removal.
pub fn install(l: &Layout, exe: &[u8], version: &str, desktop: bool) -> Result<(), String> {
    std::fs::create_dir_all(&l.dir).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(&l.dir).map_err(|e| e.to_string())?.flatten() {
        if e.path().extension().is_some_and(|x| x == "old") {
            let _ = std::fs::remove_file(e.path());
        }
    }
    if l.exe.exists() && std::fs::remove_file(&l.exe).is_err() {
        let aside = l.dir.join(format!("{EXE}.{}.old", std::process::id()));
        std::fs::rename(&l.exe, aside).map_err(|e| format!("файл программы занят: {e}"))?;
    }
    let part = l.dir.join(format!("{EXE}.part"));
    std::fs::write(&part, exe).map_err(|e| e.to_string())?;
    std::fs::rename(&part, &l.exe).map_err(|e| e.to_string())?;
    create_shortcut(&l.start_menu_link, &l.exe, APP_NAME)?;
    if desktop {
        create_shortcut(&l.desktop_link, &l.exe, APP_NAME)?;
    } else {
        let _ = std::fs::remove_file(&l.desktop_link);
    }
    let exe_s = l.exe.display().to_string();
    let key = &l.uninstall_key;
    let set = |n: &str, v: &str| crate::windows_impl::reg_set(key, n, Some(v));
    set("DisplayName", APP_NAME)?;
    set("DisplayVersion", version)?;
    set("Publisher", "DrxwnSlvt")?;
    set("DisplayIcon", &exe_s)?;
    set("InstallLocation", &l.dir.display().to_string())?;
    set("UninstallString", &format!("\"{exe_s}\" --uninstall"))?;
    set("URLInfoAbout", "https://github.com/slvx666/drxwnifyRustPC")?;
    Ok(())
}

/// Takes Mini away: shortcuts, the entry in «Apps», `drxwnify://` and start with Windows when they point at this exe,
/// then the folder (a moment after this process ends: a running exe cannot delete itself). The profile stays unless
/// `data` is given.
pub fn uninstall(l: &Layout, data: Option<&Path>) {
    let _ = std::fs::remove_file(&l.start_menu_link);
    let _ = std::fs::remove_file(&l.desktop_link);
    delete_key_tree(&l.uninstall_key);
    let exe_s = l.exe.display().to_string().to_lowercase();
    let scheme = r"Software\Classes\drxwnify";
    if crate::windows_impl::reg_get(&format!(r"{scheme}\shell\open\command"), "")
        .is_some_and(|c| c.to_lowercase().contains(&exe_s))
    {
        delete_key_tree(scheme);
    }
    if crate::windows_impl::reg_get(crate::windows_impl::RUN_KEY, APP_NAME).is_some_and(|c| c.to_lowercase().contains(&exe_s)) {
        let _ = crate::windows_impl::reg_set(crate::windows_impl::RUN_KEY, APP_NAME, None);
    }
    let mut targets = vec![l.dir.display().to_string()];
    if let Some(d) = data {
        targets.push(d.display().to_string());
    }
    let rm: String = targets.iter().map(|t| format!(" & rmdir /s /q \"{t}\"")).collect();
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // cmd reads its own quoting: the line goes as it is (Rust's argument escaping would break the paths).
    let _ = std::process::Command::new("cmd")
        .raw_arg(format!("/c ping 127.0.0.1 -n 3 >nul{rm}"))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}
