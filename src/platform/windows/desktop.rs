//! Windows desktop integration: Run-key autostart, single-instance mutex,
//! message boxes and opening files.

use std::path::Path;
use std::ptr::{null, null_mut};
use std::thread;

use log::{info, warn};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW, SW_SHOWNORMAL,
};

use super::wide;
use crate::platform::PlatformError;

pub mod autostart {
    use super::*;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey,
        RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "Perk";

    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: key opened by RegOpenKeyExW.
            unsafe { RegCloseKey(self.0) };
        }
    }

    fn open(access: u32) -> Result<Key, PlatformError> {
        let path = wide(RUN_KEY);
        let mut hkey: HKEY = null_mut();
        // SAFETY: valid pointers; key closed by `Key::drop`.
        let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, access, &mut hkey) };
        if rc != ERROR_SUCCESS {
            return Err(PlatformError::new(format!(
                "cannot open Run key (error {rc})"
            )));
        }
        Ok(Key(hkey))
    }

    pub fn is_enabled() -> bool {
        let Ok(key) = open(KEY_QUERY_VALUE) else {
            return false;
        };
        let name = wide(VALUE);
        // SAFETY: existence query with null output buffers.
        let rc = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                null(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        rc == ERROR_SUCCESS
    }

    pub fn set_enabled(enabled: bool) -> Result<(), PlatformError> {
        let key = open(KEY_SET_VALUE)?;
        let name = wide(VALUE);
        let rc = if enabled {
            let exe = std::env::current_exe()?;
            let command = wide(&format!("\"{}\"", exe.display()));
            // SAFETY: REG_SZ data is the UTF-16 buffer including its terminator.
            unsafe {
                RegSetValueExW(
                    key.0,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    command.as_ptr().cast(),
                    (command.len() * 2) as u32,
                )
            }
        } else {
            // SAFETY: valid key and value name.
            let rc = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
            if rc == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND {
                ERROR_SUCCESS
            } else {
                rc
            }
        };
        if rc != ERROR_SUCCESS {
            return Err(PlatformError::new(format!(
                "cannot update Run key (error {rc})"
            )));
        }
        info!("autostart {}", if enabled { "enabled" } else { "disabled" });
        Ok(())
    }
}

/// Named mutex held for the process lifetime; Windows releases it on exit or
/// crash.
pub struct SingleInstance {
    _handle: HANDLE,
}

impl SingleInstance {
    pub fn acquire() -> Result<Option<Self>, PlatformError> {
        let name = wide("Local\\Perk.SingleInstance");
        // SAFETY: valid name pointer; handle intentionally kept until exit.
        let handle = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(PlatformError::new(format!(
                "CreateMutexW failed ({})",
                unsafe { GetLastError() }
            )));
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            return Ok(None);
        }
        Ok(Some(SingleInstance { _handle: handle }))
    }
}

/// Show an error message box without blocking the caller.
pub fn show_error(title: &str, message: &str) -> thread::JoinHandle<()> {
    let (title, message) = (wide(title), wide(message));
    thread::spawn(move || {
        // SAFETY: valid UTF-16 strings; no owner window.
        unsafe {
            MessageBoxW(
                null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
            )
        };
    })
}

pub fn open_path(path: &Path) -> Result<(), PlatformError> {
    let verb = wide("open");
    let file = wide(&path.to_string_lossy());
    // SAFETY: valid UTF-16 strings.
    let rc = unsafe {
        ShellExecuteW(
            null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    if rc as isize > 32 {
        return Ok(());
    }
    // No file association (common for .toml / .log): fall back to Notepad.
    warn!("no handler for {}, using Notepad", path.display());
    std::process::Command::new("notepad.exe")
        .arg(path)
        .spawn()?;
    Ok(())
}
