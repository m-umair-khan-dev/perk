//! User idle time and the desktop's idle timeouts on Linux.
//!
//! Supported on GNOME (and GNOME-based desktops such as Ubuntu's): idle time
//! from Mutter's `IdleMonitor`, timeouts from GSettings. Elsewhere the idle time
//! is unknown and "when away" timers behave like normal timers.

use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use log::{debug, info};
use zbus::blocking::{Connection, Proxy};

use crate::core::away::Timeouts;

fn session_bus() -> Option<&'static Connection> {
    static BUS: OnceLock<Option<Connection>> = OnceLock::new();
    BUS.get_or_init(|| match Connection::session() {
        Ok(c) => Some(c),
        Err(e) => {
            info!("session bus unavailable, idle time unknown: {e}");
            None
        }
    })
    .as_ref()
}

/// Time since the last keyboard or mouse input.
pub fn idle_time() -> Option<Duration> {
    let proxy = Proxy::new(
        session_bus()?,
        "org.gnome.Mutter.IdleMonitor",
        "/org/gnome/Mutter/IdleMonitor/Core",
        "org.gnome.Mutter.IdleMonitor",
    )
    .ok()?;
    match proxy.call::<_, _, u64>("GetIdletime", &()) {
        Ok(ms) => Some(Duration::from_millis(ms)),
        Err(e) => {
            debug!("GetIdletime failed: {e}");
            None
        }
    }
}

/// GNOME's "Automatic suspend" and "Screen blank" settings.
pub fn sleep_timeouts(on_battery: bool) -> Option<Timeouts> {
    const POWER: &str = "org.gnome.settings-daemon.plugins.power";
    let source = if on_battery { "battery" } else { "ac" };
    let kind = gsettings(POWER, &format!("sleep-inactive-{source}-type"))?;
    let secs = seconds(&gsettings(
        POWER,
        &format!("sleep-inactive-{source}-timeout"),
    )?)?;
    let sleep = (kind != "nothing" && secs > 0).then(|| Duration::from_secs(secs));
    let display = gsettings("org.gnome.desktop.session", "idle-delay")
        .and_then(|v| seconds(&v))
        .filter(|&s| s > 0)
        .map(Duration::from_secs);
    Some(Timeouts { sleep, display })
}

/// `gsettings get` output without quotes, e.g. `suspend` or `uint32 300`.
fn gsettings(schema: &str, key: &str) -> Option<String> {
    let out = Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .trim_matches('\'')
            .to_string()
    })
}

/// The number in `1200` or `uint32 300`.
fn seconds(value: &str) -> Option<u64> {
    value.split_whitespace().last()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gsettings_numbers() {
        assert_eq!(seconds("1200"), Some(1200));
        assert_eq!(seconds("uint32 300"), Some(300));
        assert_eq!(seconds("nonsense"), None);
    }
}
