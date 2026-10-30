//! User idle time and the power plan's idle timeouts on Windows.

use std::ptr::null_mut;
use std::time::Duration;

use log::debug;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::System::Power::{
    PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows_sys::core::GUID;

use crate::core::away::Timeouts;

const GUID_SLEEP_SUBGROUP: GUID = GUID::from_u128(0x238c9fa8_0aad_41ed_83f4_97be242c8f20);
/// "Sleep after", in seconds; 0 = never.
const GUID_STANDBY_TIMEOUT: GUID = GUID::from_u128(0x29f6c1db_86da_48c5_9fdb_f2b67b1f44da);
const GUID_VIDEO_SUBGROUP: GUID = GUID::from_u128(0x7516b95f_f776_4464_8c53_06167f40cc99);
/// "Turn off the display after", in seconds; 0 = never.
const GUID_VIDEO_POWERDOWN_TIMEOUT: GUID = GUID::from_u128(0x3c0bc021_c8a8_4e07_a973_6b14cbcb2b7e);

/// Time since the last keyboard or mouse input in this session.
pub fn idle_time() -> Option<Duration> {
    // SAFETY: GetLastInputInfo fills the struct whose size we set.
    unsafe {
        let mut info = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut info) == 0 {
            return None;
        }
        // Both are 32-bit tick counts; wrapping_sub handles the 49-day wrap.
        let ms = GetTickCount().wrapping_sub(info.dwTime);
        Some(Duration::from_millis(u64::from(ms)))
    }
}

/// Sleep and display timeouts of the active power plan for the given power
/// source (Settings → System → Power).
pub fn sleep_timeouts(on_battery: bool) -> Option<Timeouts> {
    // SAFETY: PowerGetActiveScheme allocates the GUID, freed with LocalFree.
    unsafe {
        let mut scheme: *mut GUID = null_mut();
        if PowerGetActiveScheme(null_mut(), &mut scheme) != 0 || scheme.is_null() {
            debug!("no active power scheme");
            return None;
        }
        let read = |sub: &GUID, setting: &GUID| -> Option<Option<Duration>> {
            let mut value = 0u32;
            let err = if on_battery {
                PowerReadDCValueIndex(null_mut(), scheme, sub, setting, &mut value)
            } else {
                PowerReadACValueIndex(null_mut(), scheme, sub, setting, &mut value)
            };
            (err == 0).then(|| (value > 0).then(|| Duration::from_secs(u64::from(value))))
        };
        let sleep = read(&GUID_SLEEP_SUBGROUP, &GUID_STANDBY_TIMEOUT);
        let display = read(&GUID_VIDEO_SUBGROUP, &GUID_VIDEO_POWERDOWN_TIMEOUT);
        LocalFree(scheme.cast());
        Some(Timeouts {
            sleep: sleep?,
            display: display.flatten(),
        })
    }
}
