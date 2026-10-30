//! Linux keep-awake via systemd-logind plus the desktop session's own
//! inhibition API.
//!
//! - logind `Inhibit("idle", ..., "block")` returns a file descriptor; the
//!   inhibition lasts exactly as long as the fd is open, and the kernel closes
//!   it if we crash.
//! - Desktop environments run their own idle/auto-suspend policy, so we also
//!   register with GNOME's SessionManager or, elsewhere (KDE, Xfce, ...), with
//!   `org.freedesktop.ScreenSaver` / `org.freedesktop.PowerManagement`. These
//!   services drop our cookies when our D-Bus connection disappears.
//!
//! We deliberately never take logind "sleep" or "handle-lid-switch" locks, so
//! closing the lid or choosing Suspend always works, even if we misbehave.

use log::{debug, info, warn};
use zbus::blocking::{Connection, Proxy, fdo::DBusProxy};
use zbus::zvariant::OwnedFd;

use crate::platform::{InhibitOptions, Inhibitor, PlatformError};

const APP: &str = "Perk";
const REASON: &str = "Keep-awake enabled by the user";

// GNOME SessionManager inhibit flags.
const GSM_INHIBIT_SUSPEND: u32 = 4;
const GSM_INHIBIT_IDLE: u32 = 8;

#[derive(Debug, Clone, Copy)]
enum Desktop {
    Gnome,
    ScreenSaver,
    PowerManagement,
}

impl Desktop {
    fn bus_name(self) -> &'static str {
        match self {
            Desktop::Gnome => "org.gnome.SessionManager",
            Desktop::ScreenSaver => "org.freedesktop.ScreenSaver",
            Desktop::PowerManagement => "org.freedesktop.PowerManagement",
        }
    }

    fn proxy(self, conn: &Connection) -> zbus::Result<Proxy<'static>> {
        let (path, iface) = match self {
            Desktop::Gnome => ("/org/gnome/SessionManager", "org.gnome.SessionManager"),
            Desktop::ScreenSaver => (
                "/org/freedesktop/ScreenSaver",
                "org.freedesktop.ScreenSaver",
            ),
            Desktop::PowerManagement => (
                "/org/freedesktop/PowerManagement/Inhibit",
                "org.freedesktop.PowerManagement.Inhibit",
            ),
        };
        Proxy::new(conn, self.bus_name(), path, iface)
    }

    fn inhibit(self, conn: &Connection, opts: &InhibitOptions) -> zbus::Result<u32> {
        let proxy = self.proxy(conn)?;
        match self {
            Desktop::Gnome => {
                let mut flags = GSM_INHIBIT_SUSPEND;
                if opts.keep_display_on {
                    flags |= GSM_INHIBIT_IDLE;
                }
                proxy.call("Inhibit", &(APP, 0u32, REASON, flags))
            }
            Desktop::ScreenSaver | Desktop::PowerManagement => {
                proxy.call("Inhibit", &(APP, REASON))
            }
        }
    }

    fn uninhibit(self, conn: &Connection, cookie: u32) -> zbus::Result<()> {
        let method = match self {
            Desktop::Gnome => "Uninhibit",
            _ => "UnInhibit",
        };
        self.proxy(conn)?.call::<_, _, ()>(method, &(cookie,))
    }
}

#[derive(Default)]
pub struct SystemInhibitor {
    system_bus: Option<Connection>,
    session_bus: Option<Connection>,
    logind_fd: Option<OwnedFd>,
    cookies: Vec<(Desktop, u32)>,
}

impl SystemInhibitor {
    pub fn new() -> Self {
        Self::default()
    }

    fn system_bus(&mut self) -> Option<&Connection> {
        if self.system_bus.is_none() {
            match Connection::system() {
                Ok(c) => self.system_bus = Some(c),
                Err(e) => warn!("system D-Bus unavailable: {e}"),
            }
        }
        self.system_bus.as_ref()
    }

    fn session_bus(&mut self) -> Option<&Connection> {
        if self.session_bus.is_none() {
            match Connection::session() {
                Ok(c) => self.session_bus = Some(c),
                Err(e) => warn!("session D-Bus unavailable: {e}"),
            }
        }
        self.session_bus.as_ref()
    }

    fn acquire_logind(&mut self) -> Result<(), PlatformError> {
        let conn = self
            .system_bus()
            .ok_or_else(|| PlatformError::new("system D-Bus unavailable"))?;
        let proxy = Proxy::new(
            conn,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )?;
        let fd: OwnedFd = proxy.call("Inhibit", &("idle", APP, REASON, "block"))?;
        self.logind_fd = Some(fd);
        Ok(())
    }

    fn acquire_desktop(&mut self, opts: &InhibitOptions) -> Result<(), PlatformError> {
        let conn = self
            .session_bus()
            .cloned()
            .ok_or_else(|| PlatformError::new("session D-Bus unavailable"))?;
        let dbus = DBusProxy::new(&conn)?;
        let present = |d: Desktop| {
            d.bus_name()
                .try_into()
                .ok()
                .and_then(|name| dbus.name_has_owner(name).ok())
                .unwrap_or(false)
        };

        let candidates: Vec<Desktop> = if present(Desktop::Gnome) {
            vec![Desktop::Gnome]
        } else {
            let mut v = Vec::new();
            if opts.keep_display_on && present(Desktop::ScreenSaver) {
                v.push(Desktop::ScreenSaver);
            }
            if present(Desktop::PowerManagement) {
                v.push(Desktop::PowerManagement);
            }
            v
        };

        let mut last_err = None;
        for d in candidates {
            match d.inhibit(&conn, opts) {
                Ok(cookie) => {
                    debug!("{d:?} inhibit cookie {cookie}");
                    self.cookies.push((d, cookie));
                }
                Err(e) => {
                    warn!("{d:?} inhibit failed: {e}");
                    last_err = Some(e);
                }
            }
        }
        match (self.cookies.is_empty(), last_err) {
            (false, _) => Ok(()),
            (true, Some(e)) => Err(e.into()),
            (true, None) => Err(PlatformError::new("no desktop inhibition service found")),
        }
    }
}

impl Inhibitor for SystemInhibitor {
    fn acquire(&mut self, opts: &InhibitOptions) -> Result<(), PlatformError> {
        self.release();
        let logind = self.acquire_logind();
        let desktop = self.acquire_desktop(opts);
        match (&logind, &desktop) {
            (Err(a), Err(b)) => {
                return Err(PlatformError::new(format!(
                    "no inhibition mechanism available (logind: {a}; desktop: {b})"
                )));
            }
            (Err(e), Ok(())) => warn!("logind inhibit unavailable: {e}"),
            (Ok(()), Err(e)) => warn!("desktop inhibit unavailable, relying on logind only: {e}"),
            (Ok(()), Ok(())) => {}
        }
        info!(
            "keep-awake acquired (logind: {}, desktop: {:?})",
            self.logind_fd.is_some(),
            self.cookies.iter().map(|(d, _)| d).collect::<Vec<_>>()
        );
        Ok(())
    }

    fn release(&mut self) {
        if self.logind_fd.take().is_some() {
            debug!("logind inhibitor fd closed");
        }
        if !self.cookies.is_empty() {
            let cookies = std::mem::take(&mut self.cookies);
            if let Some(conn) = self.session_bus.clone() {
                for (d, cookie) in cookies {
                    if let Err(e) = d.uninhibit(&conn, cookie) {
                        // Dropping the connection is the fallback: services
                        // release cookies when their owner disconnects.
                        warn!("{d:?} uninhibit failed: {e}; resetting session connection");
                        self.session_bus = None;
                    }
                }
            }
            info!("keep-awake released");
        }
    }

    fn is_held(&self) -> bool {
        self.logind_fd.is_some() || !self.cookies.is_empty()
    }
}

impl Drop for SystemInhibitor {
    fn drop(&mut self) {
        self.release();
    }
}
