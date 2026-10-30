//! Windows keep-awake via `SetThreadExecutionState`.
//!
//! The execution state belongs to the calling thread and is cleared by the OS
//! when that thread (or the process) ends, so a crash can never leave the
//! machine stuck awake. It also never blocks lid-close or user-initiated
//! sleep: it only resets the idle timers.

use std::marker::PhantomData;

use log::info;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};

use crate::platform::{InhibitOptions, Inhibitor, PlatformError};

pub struct SystemInhibitor {
    held: bool,
    /// Execution state is per-thread: keep this type on the thread that made it.
    _not_send: PhantomData<*const ()>,
}

impl SystemInhibitor {
    pub fn new() -> Self {
        SystemInhibitor {
            held: false,
            _not_send: PhantomData,
        }
    }
}

impl Default for SystemInhibitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Inhibitor for SystemInhibitor {
    fn acquire(&mut self, opts: &InhibitOptions) -> Result<(), PlatformError> {
        let mut flags = ES_CONTINUOUS | ES_SYSTEM_REQUIRED;
        if opts.keep_display_on {
            flags |= ES_DISPLAY_REQUIRED;
        }
        // SAFETY: plain Win32 call with valid flags.
        if unsafe { SetThreadExecutionState(flags) } == 0 {
            let code = unsafe { GetLastError() };
            return Err(PlatformError::new(format!(
                "SetThreadExecutionState failed (error {code})"
            )));
        }
        self.held = true;
        info!("keep-awake acquired (display: {})", opts.keep_display_on);
        Ok(())
    }

    fn release(&mut self) {
        // SAFETY: plain Win32 call; ES_CONTINUOUS alone clears our requirements.
        unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        if std::mem::replace(&mut self.held, false) {
            info!("keep-awake released");
        }
    }

    fn is_held(&self) -> bool {
        self.held
    }
}

impl Drop for SystemInhibitor {
    fn drop(&mut self) {
        self.release();
    }
}
