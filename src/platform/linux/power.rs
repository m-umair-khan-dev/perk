//! Event-driven power notifications on Linux.
//!
//! - Lid: UPower's `LidIsClosed` property, whose changes are pushed as D-Bus
//!   `PropertiesChanged` signals (logind's `LidClosed` property does not emit
//!   change signals, so it would require polling).
//! - Power source / battery: UPower's `OnBattery` and the `DisplayDevice`
//!   (the combined battery) `Percentage`, also pushed as change signals.
//! - Suspend/resume: logind's `PrepareForSleep(bool)` signal (covers s2idle,
//!   deep sleep and hibernate alike).
//!
//! No polling anywhere: each listener blocks on its D-Bus signal stream.

use std::sync::Arc;
use std::thread;

use log::{error, info, warn};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

use crate::platform::{LidSupport, PlatformError, PowerEvent, PowerInfo, PowerSink};

const UPOWER: &str = "org.freedesktop.UPower";

pub fn start_power_monitor(sink: PowerSink) -> Result<PowerInfo, PlatformError> {
    let sink: Arc<PowerSink> = Arc::new(sink);
    let conn = Connection::system()?;

    if let Err(e) = start_sleep_listener(&conn, sink.clone()) {
        warn!("suspend notifications unavailable: {e}");
    }

    let lid = match start_lid_listener(&conn, sink.clone()) {
        Ok(support) => support,
        Err(e) => {
            warn!("lid monitoring unavailable: {e}");
            LidSupport::Unavailable("UPower is not running; lid state cannot be observed".into())
        }
    };

    let battery_monitored = match start_battery_listeners(&conn, sink) {
        Ok(present) => present,
        Err(e) => {
            warn!("battery monitoring unavailable: {e}");
            false
        }
    };
    info!("power monitoring: lid={lid:?} battery={battery_monitored}");
    Ok(PowerInfo {
        lid,
        battery_monitored,
    })
}

fn upower_proxy(
    conn: &Connection,
    path: &'static str,
    iface: &'static str,
) -> zbus::Result<Proxy<'static>> {
    Proxy::new(conn, UPOWER, path, iface)
}

/// Send the current value of a property, then every change, mapped to events.
fn watch_property<T>(
    proxy: Proxy<'static>,
    name: &'static str,
    sink: Arc<PowerSink>,
    map: fn(T) -> Option<PowerEvent>,
) -> zbus::Result<()>
where
    T: TryFrom<OwnedValue> + Unpin + Send + 'static,
    T::Error: Into<zbus::Error>,
{
    // Subscribe before reading the initial value so no change is missed.
    let changes = proxy.receive_property_changed::<T>(name);
    if let Some(event) = map(proxy.get_property::<T>(name)?) {
        sink(event);
    }
    thread::Builder::new()
        .name(format!("perk-{name}"))
        .spawn(move || {
            let _keep_alive = &proxy;
            for change in changes {
                match change.get() {
                    Ok(value) => {
                        if let Some(event) = map(value) {
                            sink(event);
                        }
                    }
                    Err(e) => warn!("bad {name} update: {e}"),
                }
            }
            error!("UPower {name} signal stream ended");
        })
        .map_err(|e| zbus::Error::Failure(e.to_string()))?;
    Ok(())
}

fn start_lid_listener(conn: &Connection, sink: Arc<PowerSink>) -> zbus::Result<LidSupport> {
    let upower = upower_proxy(conn, "/org/freedesktop/UPower", UPOWER)?;
    if !upower.get_property::<bool>("LidIsPresent")? {
        info!("no lid present; lid monitoring not needed");
        return Ok(LidSupport::NoLid);
    }
    watch_property::<bool>(upower, "LidIsClosed", sink, |closed| {
        Some(if closed {
            PowerEvent::LidClosed
        } else {
            PowerEvent::LidOpened
        })
    })?;
    Ok(LidSupport::Monitored)
}

/// Returns whether a battery is present.
fn start_battery_listeners(conn: &Connection, sink: Arc<PowerSink>) -> zbus::Result<bool> {
    let upower = upower_proxy(conn, "/org/freedesktop/UPower", UPOWER)?;
    watch_property::<bool>(upower, "OnBattery", sink.clone(), |on| {
        Some(PowerEvent::OnBattery(on))
    })?;

    let battery = upower_proxy(
        conn,
        "/org/freedesktop/UPower/devices/DisplayDevice",
        "org.freedesktop.UPower.Device",
    )?;
    if !battery.get_property::<bool>("IsPresent").unwrap_or(false) {
        info!("no battery present");
        return Ok(false);
    }
    watch_property::<f64>(battery, "Percentage", sink, |p| {
        Some(PowerEvent::BatteryLevel(p.round().clamp(0.0, 100.0) as u8))
    })?;
    Ok(true)
}

fn start_sleep_listener(conn: &Connection, sink: Arc<PowerSink>) -> zbus::Result<()> {
    let logind = Proxy::new(
        conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;
    let signals = logind.receive_signal("PrepareForSleep")?;
    thread::Builder::new()
        .name("perk-sleep".into())
        .spawn(move || {
            let _keep_alive = &logind;
            for msg in signals {
                match msg.body().deserialize::<bool>() {
                    Ok(true) => sink(PowerEvent::Suspending),
                    Ok(false) => sink(PowerEvent::Resumed),
                    Err(e) => warn!("bad PrepareForSleep signal: {e}"),
                }
            }
            error!("logind sleep signal stream ended");
        })
        .map_err(|e| zbus::Error::Failure(e.to_string()))?;
    Ok(())
}
