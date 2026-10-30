// No console window for release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use perk::config::Config;
use perk::platform::{self, SingleInstance};
use perk::{app, logging};

const HELP: &str = "\
Perk — keep your computer awake, safely.

Usage: perk [--help | --version]

Perk runs in the system tray. Settings live in:
  Windows: %APPDATA%\\perk\\config.toml
  Linux:   ~/.config/perk/config.toml

Environment:
  PERK_LOG=debug   more detailed logging (error, warn, info, debug, trace)
";

fn main() {
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "-h" | "--help" => print!("{HELP}"),
            "-V" | "--version" => println!("perk {}", env!("CARGO_PKG_VERSION")),
            other => {
                eprintln!("perk: unknown argument '{other}'\n\n{HELP}");
                std::process::exit(2);
            }
        }
        return;
    }

    // A tray icon and a menu never need OpenGL, but GTK 3 sets it up at
    // start-up anyway, which loads the Mesa driver (with software rendering:
    // ~50 MB of LLVM). Turning it off cuts memory use by about two thirds.
    #[cfg(target_os = "linux")]
    if std::env::var_os("GDK_GL").is_none() {
        // SAFETY: no other threads exist yet, so nothing reads the
        // environment concurrently.
        unsafe { std::env::set_var("GDK_GL", "disable") };
    }

    // Check for a running instance before touching the log file it owns.
    let _instance = match SingleInstance::acquire() {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => {
            eprintln!("Perk is already running.");
            let msg = "Perk is already running.\n\nLook for the cup icon in the system tray.";
            let _ = platform::show_error("Perk", msg).join();
            std::process::exit(1);
        }
        Err(e) => {
            // Not fatal: a second instance would only hold its own,
            // independently released inhibition.
            eprintln!("perk: single-instance check failed: {e}");
            None
        }
    };

    let log_path = logging::default_path();
    logging::init(log_path.as_deref());
    app::run(Config::default_path(), log_path);
}
