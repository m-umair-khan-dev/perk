//! Linux desktop integration: autostart entry, single-instance lock, and
//! small helpers for prompting and opening files.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use log::{info, warn};

use crate::platform::PlatformError;

pub mod autostart {
    use super::*;

    fn entry_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("autostart").join("perk.desktop"))
    }

    pub fn is_enabled() -> bool {
        entry_path().is_some_and(|p| p.exists())
    }

    pub fn set_enabled(enabled: bool) -> Result<(), PlatformError> {
        let path = entry_path().ok_or_else(|| PlatformError::new("no config directory"))?;
        if enabled {
            let exe = std::env::current_exe()?;
            fs::create_dir_all(path.parent().expect("autostart dir"))?;
            fs::write(&path, desktop_entry(&exe))?;
            info!("autostart enabled: {}", path.display());
        } else if path.exists() {
            fs::remove_file(&path)?;
            info!("autostart disabled");
        }
        Ok(())
    }

    pub(crate) fn desktop_entry(exe: &Path) -> String {
        // Desktop Entry spec: quote the path and escape reserved characters.
        let mut quoted = String::from("\"");
        for c in exe.to_string_lossy().chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(c);
        }
        quoted.push('"');
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Perk\n\
             Comment=Keep your computer awake\n\
             Exec={quoted}\n\
             Icon=perk\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n"
        )
    }
}

/// Holds an exclusive `flock` for the lifetime of the process. The kernel
/// drops the lock if the process dies, so a crash never blocks a restart.
pub struct SingleInstance {
    _file: File,
}

impl SingleInstance {
    /// `Ok(None)` if another instance already holds the lock.
    pub fn acquire() -> Result<Option<Self>, PlatformError> {
        let dir = dirs::runtime_dir()
            .or_else(dirs::cache_dir)
            .unwrap_or_else(std::env::temp_dir);
        fs::create_dir_all(&dir)?;
        let path = dir.join("perk.lock");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        // SAFETY: valid open fd owned by `file`.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Ok(None);
            }
            return Err(err.into());
        }
        let _ = file.set_len(0);
        let _ = writeln!(file, "{}", std::process::id());
        Ok(Some(SingleInstance { _file: file }))
    }
}

/// Ask the user for a duration with the desktop's own dialog tool (zenity or
/// kdialog). Runs on a worker thread; `done` gets `None` if cancelled or if no
/// dialog tool is installed (the caller then falls back to the settings file).
pub fn prompt_duration(
    title: &str,
    message: &str,
    initial: &str,
    done: Box<dyn FnOnce(Option<String>, bool) + Send>,
) {
    let (title, message, initial) = (title.to_owned(), message.to_owned(), initial.to_owned());
    thread::spawn(move || {
        let attempts: [(&str, Vec<&str>); 2] = [
            (
                "zenity",
                vec![
                    "--entry",
                    "--title",
                    &title,
                    "--text",
                    &message,
                    "--entry-text",
                    &initial,
                ],
            ),
            (
                "kdialog",
                vec!["--title", &title, "--inputbox", &message, &initial],
            ),
        ];
        for (tool, args) in attempts {
            match Command::new(tool).args(args).stderr(Stdio::null()).output() {
                Ok(out) => {
                    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    let value = (out.status.success() && !text.is_empty()).then_some(text);
                    return done(value, true);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    warn!("{tool} failed: {e}");
                    continue;
                }
            }
        }
        warn!("neither zenity nor kdialog is installed");
        done(None, false);
    });
}

/// Show a short error message using the desktop's dialog tool, if any.
pub fn show_error(title: &str, message: &str) -> thread::JoinHandle<()> {
    let (title, message) = (title.to_owned(), message.to_owned());
    thread::spawn(move || {
        let ok = Command::new("zenity")
            .args(["--error", "--title", &title, "--text", &message])
            .stderr(Stdio::null())
            .status()
            .is_ok();
        if !ok {
            let _ = Command::new("kdialog")
                .args(["--title", &title, "--error", &message])
                .stderr(Stdio::null())
                .status();
        }
    })
}

/// Set by [`disable_gtk_gl`] when Perk (not the user) turned GTK's OpenGL off.
static GL_DISABLED_BY_PERK: AtomicBool = AtomicBool::new(false);

/// Stop GTK from setting up OpenGL in this process: a tray icon and menu never
/// need it, and it loads the Mesa driver (with software rendering, ~50 MB of
/// LLVM). Only affects Perk; apps it opens get the setting removed again.
/// A `GDK_GL` value set by the user is left alone.
///
/// Call before any other thread starts.
pub fn disable_gtk_gl() {
    if std::env::var_os("GDK_GL").is_none() {
        // SAFETY: called at start-up, before other threads read the environment.
        unsafe { std::env::set_var("GDK_GL", "disable") };
        GL_DISABLED_BY_PERK.store(true, Ordering::Relaxed);
    }
}

pub fn open_path(path: &Path) -> Result<(), PlatformError> {
    spawn_detached("xdg-open", path.as_os_str())
}

fn spawn_detached(program: &str, arg: &OsStr) -> Result<(), PlatformError> {
    let mut command = Command::new(program);
    // The opened app (e.g. a text editor) may outlive Perk: give it the
    // user's normal graphics setup.
    if GL_DISABLED_BY_PERK.load(Ordering::Relaxed) {
        command.env_remove("GDK_GL");
    }
    let mut child = command
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Reap in the background so no zombie is left behind.
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_quotes_exec() {
        let entry = autostart::desktop_entry(Path::new("/opt/my apps/pe\"rk"));
        assert!(entry.contains("Exec=\"/opt/my apps/pe\\\"rk\"\n"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
    }
}
