//! User settings, stored as TOML in the platform config directory
//! (`%APPDATA%\perk\config.toml` or `~/.config/perk/config.toml`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::core::duration::MAX_MINUTES;
use crate::core::{Mode, Policy};
use crate::platform::InhibitOptions;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Keep the display on and block the idle screen lock (not just sleep).
    pub keep_display_on: bool,
    /// Continue the previous keep-awake session after the lid is reopened or
    /// the system wakes up.
    pub resume_after_lid_open: bool,
    /// Custom duration in minutes (1 to 10080).
    pub custom_minutes: u32,
    /// Duration used by the plain on/off toggle; 0 means indefinitely.
    pub last_minutes: u32,
    /// Pause keep-awake while on battery at or below this charge (percent).
    /// 0 disables the check.
    pub pause_below_battery_percent: u8,
    /// Global shortcut for the emergency stop; empty disables it.
    pub emergency_hotkey: String,
    /// Timed sessions count down only while you are away from the computer,
    /// and Perk steps in just before the computer would go to sleep.
    pub start_timer_when_away: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            keep_display_on: true,
            resume_after_lid_open: true,
            custom_minutes: 45,
            last_minutes: 0,
            pause_below_battery_percent: 10,
            emergency_hotkey: "Ctrl+Alt+Shift+K".into(),
            start_timer_when_away: false,
        }
    }
}

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("perk").join("config.toml"))
    }

    /// Load from `path`. A missing file yields defaults (and writes them so the
    /// file is easy to find); a broken file yields defaults but is left alone.
    pub fn load_or_create(path: &Path) -> Config {
        match fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => {
                    let cfg = cfg.sanitized();
                    // Add options introduced by newer versions to an older
                    // file (existing values are kept), so they are discoverable.
                    if has_missing_keys(&text) {
                        if let Err(e) = cfg.save(path) {
                            warn!("could not add new settings to config: {e}");
                        }
                    }
                    cfg
                }
                Err(e) => {
                    warn!("ignoring invalid config {}: {e}", path.display());
                    Config::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                if let Err(e) = cfg.save(path) {
                    warn!("could not write default config: {e}");
                }
                cfg
            }
            Err(e) => {
                warn!("could not read config {}: {e}", path.display());
                Config::default()
            }
        }
    }

    /// Atomic save: write a temp file next to the target, then rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let body = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(HEADER.as_bytes())?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        info!("saved settings to {}", path.display());
        Ok(())
    }

    fn sanitized(mut self) -> Self {
        self.custom_minutes = self.custom_minutes.clamp(1, MAX_MINUTES);
        self.last_minutes = self.last_minutes.min(MAX_MINUTES);
        self.pause_below_battery_percent = self.pause_below_battery_percent.min(100);
        self
    }

    pub fn policy(&self) -> Policy {
        Policy {
            resume_after_pause: self.resume_after_lid_open,
            low_battery_percent: self.pause_below_battery_percent,
        }
    }

    pub fn inhibit_options(&self) -> InhibitOptions {
        InhibitOptions {
            keep_display_on: self.keep_display_on,
        }
    }

    pub fn last_mode(&self) -> Mode {
        Mode::from_minutes(self.last_minutes)
    }
}

fn has_missing_keys(text: &str) -> bool {
    let Ok(file) = text.parse::<toml::Table>() else {
        return false;
    };
    let Ok(toml::Value::Table(all)) = toml::Value::try_from(Config::default()) else {
        return false;
    };
    all.keys().any(|k| !file.contains_key(k))
}

const HEADER: &str = "\
# Perk settings. Most options are also available from the tray menu.
# Changes made here are picked up on restart, or via \"Reload settings\".

";

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("perk-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("config.toml")
    }

    #[test]
    fn missing_file_creates_defaults() {
        let path = temp_path("missing");
        let cfg = Config::load_or_create(&path);
        assert_eq!(cfg, Config::default());
        assert!(path.exists());
        assert_eq!(Config::load_or_create(&path), Config::default());
    }

    #[test]
    fn round_trips() {
        let path = temp_path("roundtrip");
        let cfg = Config {
            keep_display_on: false,
            custom_minutes: 90,
            last_minutes: 120,
            ..Config::default()
        };
        cfg.save(&path).unwrap();
        assert_eq!(Config::load_or_create(&path), cfg);
    }

    #[test]
    fn partial_file_fills_defaults_and_clamps() {
        let path = temp_path("partial");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "custom_minutes = 999999\n").unwrap();
        let cfg = Config::load_or_create(&path);
        assert_eq!(cfg.custom_minutes, MAX_MINUTES);
        assert!(cfg.keep_display_on);
    }

    #[test]
    fn older_file_gains_new_keys_and_keeps_values() {
        let path = temp_path("upgrade");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "custom_minutes = 90
",
        )
        .unwrap();
        let cfg = Config::load_or_create(&path);
        assert_eq!(cfg.custom_minutes, 90);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("custom_minutes = 90"));
        assert!(text.contains("pause_below_battery_percent = 10"));
    }

    #[test]
    fn invalid_file_is_not_overwritten() {
        let path = temp_path("invalid");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "this is = = not toml").unwrap();
        assert_eq!(Config::load_or_create(&path), Config::default());
        assert_eq!(fs::read_to_string(&path).unwrap(), "this is = = not toml");
    }
}
