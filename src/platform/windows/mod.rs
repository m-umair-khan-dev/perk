mod desktop;
mod idle;
mod inhibit;
mod power;
mod prompt;

pub use desktop::{SingleInstance, autostart, open_path, show_error};
pub use idle::{idle_time, sleep_timeouts};
pub use inhibit::SystemInhibitor;
pub use power::start_power_monitor;
pub use prompt::prompt_duration;

/// NUL-terminated UTF-16 for Win32 APIs.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
