//! OS integration behind small, testable interfaces.
//!
//! Each platform module provides:
//! - `SystemInhibitor`: an [`Inhibitor`] using native power-management APIs.
//! - `start_power_monitor`: event-driven lid / suspend notifications.
//! - `autostart`: launch-at-login management.
//! - `SingleInstance`: process-wide single-instance guard.
//! - `prompt_duration` / `open_path`: small desktop helpers.
//! - `idle_time` / `sleep_timeouts`: user idle time and the OS idle timeouts.
//!
//! All inhibition handles are owned by the OS on behalf of this process
//! (thread execution state on Windows, file descriptors and D-Bus connections
//! on Linux), so they are released automatically if the process crashes.

use std::fmt;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(not(any(windows, target_os = "linux")))]
compile_error!("Perk supports Windows and Linux only.");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InhibitOptions {
    /// Also keep the display on and prevent the idle screen lock.
    /// When false, only automatic system sleep is prevented.
    pub keep_display_on: bool,
}

impl Default for InhibitOptions {
    fn default() -> Self {
        InhibitOptions {
            keep_display_on: true,
        }
    }
}

/// Holds (or not) an OS-level "stay awake" request.
pub trait Inhibitor {
    fn acquire(&mut self, opts: &InhibitOptions) -> Result<(), PlatformError>;
    /// Release everything this process holds. Must be idempotent and safe to
    /// call when nothing is held (used by the emergency stop).
    fn release(&mut self);
    fn is_held(&self) -> bool;
}

/// Power events delivered by the platform monitor, from any thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    LidClosed,
    LidOpened,
    Suspending,
    Resumed,
    /// `true` when running on battery power.
    OnBattery(bool),
    /// Remaining battery charge in percent.
    BatteryLevel(u8),
}

pub type PowerSink = Box<dyn Fn(PowerEvent) + Send + Sync + 'static>;

/// What the power monitor found out about this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerInfo {
    pub lid: LidSupport,
    /// Battery state is being reported (false on desktops / no UPower).
    pub battery_monitored: bool,
}

/// How well lid state can be observed on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LidSupport {
    Monitored,
    NoLid,
    Unavailable(String),
}

#[derive(Debug, Clone)]
pub struct PlatformError(String);

impl PlatformError {
    pub fn new(msg: impl Into<String>) -> Self {
        PlatformError(msg.into())
    }
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlatformError {}

#[cfg(target_os = "linux")]
impl From<zbus::Error> for PlatformError {
    fn from(e: zbus::Error) -> Self {
        PlatformError(e.to_string())
    }
}

impl From<std::io::Error> for PlatformError {
    fn from(e: std::io::Error) -> Self {
        PlatformError(e.to_string())
    }
}
