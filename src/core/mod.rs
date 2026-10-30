//! Platform-independent keep-awake logic.

pub mod away;
pub mod controller;
pub mod duration;
pub mod state;

pub use controller::Controller;
pub use state::{Effective, IdleSample, Input, Mode, PauseReason, Policy, Session};
