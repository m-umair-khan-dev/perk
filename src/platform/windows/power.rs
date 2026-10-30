//! Event-driven power notifications on Windows.
//!
//! A hidden window on a dedicated thread registers power-setting notifications
//! and receives `WM_POWERBROADCAST` messages:
//! - `PBT_POWERSETTINGCHANGE` for the lid switch, AC/DC power source and
//!   battery percentage. Windows sends each one once on registration with the
//!   current value, then on every change, so no polling is needed.
//! - `PBT_APMSUSPEND` / `PBT_APMRESUMEAUTOMATIC` around classic (S3/S4) sleep.
//!   Modern Standby (S0ix) machines do not send these; there the lid
//!   notification is what protects the machine, and it works identically.

use std::cell::RefCell;
use std::ptr::{null, null_mut};
use std::sync::mpsc;
use std::thread;

use log::{error, info, warn};
use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::{
    GetPwrCapabilities, POWERBROADCAST_SETTING, RegisterPowerSettingNotification,
    SYSTEM_POWER_CAPABILITIES,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DispatchMessageW, GetMessageW,
    MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, PBT_POWERSETTINGCHANGE, RegisterClassW,
    TranslateMessage, WM_POWERBROADCAST, WNDCLASSW, WS_OVERLAPPED,
};
use windows_sys::core::GUID;

use super::wide;
use crate::platform::{LidSupport, PlatformError, PowerEvent, PowerInfo, PowerSink};

/// {BA3E0F4D-B817-4094-A2D1-D56379E6A0F3}: DWORD, 0 = closed, 1 = open.
const GUID_LIDSWITCH_STATE_CHANGE: GUID = GUID::from_u128(0xBA3E0F4D_B817_4094_A2D1_D56379E6A0F3);
/// {5D3E9A59-E9D5-4B00-A6BD-FF34FF516548}: DWORD, 0 = AC, 1 = battery, 2 = UPS.
const GUID_ACDC_POWER_SOURCE: GUID = GUID::from_u128(0x5D3E9A59_E9D5_4B00_A6BD_FF34FF516548);
/// {A7AD8041-B45A-4CAE-87A3-EECBB468A9E1}: DWORD, percent remaining.
const GUID_BATTERY_PERCENTAGE_REMAINING: GUID =
    GUID::from_u128(0xA7AD8041_B45A_4CAE_87A3_EECBB468A9E1);

thread_local! {
    static SINK: RefCell<Option<PowerSink>> = const { RefCell::new(None) };
}

/// What this machine supports, from `GetPwrCapabilities`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Capabilities {
    lid: bool,
    modern_standby: bool,
    classic_sleep: bool,
    hibernate: bool,
    battery: bool,
}

fn capabilities() -> Option<Capabilities> {
    // SAFETY: GetPwrCapabilities fills the zeroed struct we pass in.
    unsafe {
        let mut caps: SYSTEM_POWER_CAPABILITIES = std::mem::zeroed();
        if GetPwrCapabilities(&mut caps) == 0 {
            warn!("GetPwrCapabilities failed ({})", GetLastError());
            return None;
        }
        Some(Capabilities {
            lid: caps.LidPresent != 0,
            modern_standby: caps.AoAc != 0,
            classic_sleep: caps.SystemS3 != 0,
            hibernate: caps.SystemS4 != 0 && caps.HiberFilePresent != 0,
            battery: caps.SystemBatteriesPresent != 0,
        })
    }
}

pub fn start_power_monitor(sink: PowerSink) -> Result<PowerInfo, PlatformError> {
    let caps = capabilities();
    match caps {
        Some(c) => info!(
            "power capabilities: lid={} modern_standby={} s3_sleep={} hibernate={} battery={}",
            c.lid, c.modern_standby, c.classic_sleep, c.hibernate, c.battery
        ),
        None => warn!("power capabilities unknown; assuming a laptop"),
    }

    let (ready_tx, ready_rx) = mpsc::channel();
    thread::Builder::new()
        .name("perk-power".into())
        .spawn(move || {
            SINK.with(|s| *s.borrow_mut() = Some(sink));
            match create_window() {
                Ok(_hwnd) => {
                    let _ = ready_tx.send(Ok(()));
                    run_message_loop();
                    error!("power notification loop ended");
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })?;
    ready_rx
        .recv()
        .map_err(|_| PlatformError::new("power monitor thread died"))??;
    info!("power monitoring via WM_POWERBROADCAST");

    // Unknown capabilities: keep monitoring and treat it as a laptop (safe side).
    let lid = match caps {
        Some(c) if !c.lid => LidSupport::NoLid,
        _ => LidSupport::Monitored,
    };
    Ok(PowerInfo {
        lid,
        battery_monitored: caps.is_none_or(|c| c.battery),
    })
}

fn create_window() -> Result<HWND, PlatformError> {
    let class = wide("PerkPowerMonitor");
    // SAFETY: standard window class registration and creation on this thread.
    unsafe {
        let hinstance = GetModuleHandleW(null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(PlatformError::new(format!(
                "RegisterClassW failed ({})",
                GetLastError()
            )));
        }
        // A hidden top-level window (not message-only): power broadcasts are
        // only delivered to top-level windows.
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinstance,
            null(),
        );
        if hwnd.is_null() {
            return Err(PlatformError::new(format!(
                "CreateWindowExW failed ({})",
                GetLastError()
            )));
        }
        for (name, guid) in [
            ("lid switch", &GUID_LIDSWITCH_STATE_CHANGE),
            ("power source", &GUID_ACDC_POWER_SOURCE),
            ("battery level", &GUID_BATTERY_PERCENTAGE_REMAINING),
        ] {
            // Handles live as long as the window; the process never unregisters.
            if RegisterPowerSettingNotification(hwnd, guid, DEVICE_NOTIFY_WINDOW_HANDLE) == 0 {
                warn!("cannot subscribe to {name} changes ({})", GetLastError());
            }
        }
        Ok(hwnd)
    }
}

fn run_message_loop() {
    // SAFETY: standard message loop for windows owned by this thread.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn emit(event: PowerEvent) {
    SINK.with(|s| {
        if let Some(sink) = s.borrow().as_ref() {
            sink(event);
        }
    });
}

/// Map a power-setting notification to an event.
fn setting_event(guid: &GUID, value: u32) -> Option<PowerEvent> {
    if guid_eq(guid, &GUID_LIDSWITCH_STATE_CHANGE) {
        Some(if value == 0 {
            PowerEvent::LidClosed
        } else {
            PowerEvent::LidOpened
        })
    } else if guid_eq(guid, &GUID_ACDC_POWER_SOURCE) {
        // 1 = battery, 2 = short-term (UPS) battery: both drain.
        Some(PowerEvent::OnBattery(value != 0))
    } else if guid_eq(guid, &GUID_BATTERY_PERCENTAGE_REMAINING) {
        Some(PowerEvent::BatteryLevel(value.min(100) as u8))
    } else {
        None
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_POWERBROADCAST {
        match wparam as u32 {
            PBT_POWERSETTINGCHANGE if lparam != 0 => {
                // SAFETY: for PBT_POWERSETTINGCHANGE, lparam points to a valid
                // POWERBROADCAST_SETTING followed by DataLength bytes of data.
                unsafe {
                    let setting = &*(lparam as *const POWERBROADCAST_SETTING);
                    let len = setting.DataLength as usize;
                    let data = setting.Data.as_ptr();
                    let value = if len >= 4 {
                        data.cast::<u32>().read_unaligned()
                    } else if len >= 1 {
                        u32::from(*data)
                    } else {
                        return 1;
                    };
                    if let Some(event) = setting_event(&setting.PowerSetting, value) {
                        emit(event);
                    }
                }
            }
            PBT_APMSUSPEND => emit(PowerEvent::Suspending),
            PBT_APMRESUMEAUTOMATIC => emit(PowerEvent::Resumed),
            _ => {}
        }
        return 1; // TRUE
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn guid_eq(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}
