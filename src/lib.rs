//! Perk: a battery-safe keep-awake utility for Windows and Linux.
//!
//! - [`core`]: platform-independent state machine and controller.
//! - [`platform`]: native inhibition, lid/suspend events, autostart.
//! - [`app`]: tray UI and event loop.

pub mod app;
pub mod config;
pub mod core;
pub mod icon;
pub mod logging;
pub mod platform;
