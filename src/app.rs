//! Tray application: owns the event loop, the tray icon/menu and the
//! controller. Everything here runs on the main thread; OS notifications
//! arrive from platform threads through the event loop proxy.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use log::{error, info, warn};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::config::Config;
use crate::core::away::{self, LEAD};
use crate::core::duration::{describe_minutes, format_remaining, parse_minutes};
use crate::core::{Controller, Effective, IdleSample, Input, Mode, PauseReason};
use crate::icon::{self, IconKind};
use crate::platform::{self, LidSupport, PowerEvent, PowerInfo, SystemInhibitor};

const PRESETS: [(u32, &str); 3] = [(30, "30 minutes"), (60, "1 hour"), (120, "2 hours")];
const PROMPT_TITLE: &str = "Perk — custom duration";
const PROMPT_TEXT: &str = "Keep awake for how long?\nExamples: 45m, 2h, 1h30m, 1:30";
const AWAY_LABEL: &str = "Start timer when I step away";

#[derive(Debug)]
enum UserEvent {
    Tray(TrayIconEvent),
    Menu(MenuEvent),
    Power(PowerEvent),
    HotKey(GlobalHotKeyEvent),
    CustomDuration {
        input: Option<String>,
        dialog_available: bool,
    },
}

struct Icons {
    active: Icon,
    paused: Icon,
    waiting: Icon,
    off: Icon,
}

impl Icons {
    fn new() -> Self {
        let make = |kind| {
            let size = icon::TRAY_SIZE;
            Icon::from_rgba(icon::render(kind, size), size, size).expect("valid icon")
        };
        Icons {
            active: make(IconKind::Active),
            paused: make(IconKind::Paused),
            waiting: make(IconKind::Waiting),
            off: make(IconKind::Off),
        }
    }

    fn get(&self, kind: IconKind) -> Icon {
        match kind {
            IconKind::Active | IconKind::Logo => self.active.clone(),
            IconKind::Paused => self.paused.clone(),
            IconKind::Waiting => self.waiting.clone(),
            IconKind::Off => self.off.clone(),
        }
    }
}

struct Menus {
    menu: Menu,
    status: MenuItem,
    detail: MenuItem,
    detail_shown: bool,
    toggle: CheckMenuItem,
    indefinite: CheckMenuItem,
    presets: Vec<(u32, CheckMenuItem)>,
    custom: CheckMenuItem,
    set_custom: MenuItem,
    away: CheckMenuItem,
    emergency: MenuItem,
    autostart: CheckMenuItem,
    display: CheckMenuItem,
    resume: CheckMenuItem,
    open_config: MenuItem,
    reload: MenuItem,
    open_log: MenuItem,
    quit: MenuItem,
}

impl Menus {
    fn build(cfg: &Config) -> Result<Self, tray_icon::menu::Error> {
        let status = MenuItem::new("Perk", false, None);
        let detail = MenuItem::new("", false, None);
        let toggle = CheckMenuItem::new("Keep awake", true, false, None);
        let indefinite = CheckMenuItem::new("Indefinitely", true, false, None);
        let presets: Vec<_> = PRESETS
            .iter()
            .map(|&(m, label)| (m, CheckMenuItem::new(label, true, false, None)))
            .collect();
        let custom = CheckMenuItem::new(custom_label(cfg.custom_minutes), true, false, None);
        let set_custom = MenuItem::new("Set custom duration…", true, None);
        let away = CheckMenuItem::new(AWAY_LABEL, true, cfg.start_timer_when_away, None);
        let emergency = MenuItem::new("Emergency stop", true, None);
        let autostart = CheckMenuItem::new(
            "Launch at startup",
            true,
            platform::autostart::is_enabled(),
            None,
        );
        let display = CheckMenuItem::new("Keep display on", true, cfg.keep_display_on, None);
        let resume = CheckMenuItem::new(
            "Resume when lid reopens",
            true,
            cfg.resume_after_lid_open,
            None,
        );
        let open_config = MenuItem::new("Open settings file", true, None);
        let reload = MenuItem::new("Reload settings", true, None);
        let open_log = MenuItem::new("Open log file", true, None);
        let quit = MenuItem::new("Quit Perk", true, None);

        let sep = PredefinedMenuItem::separator;
        let mut duration_items: Vec<&dyn IsMenuItem> = vec![&indefinite];
        duration_items.extend(presets.iter().map(|(_, item)| item as &dyn IsMenuItem));
        let sep_d = sep();
        let sep_a = sep();
        duration_items.extend([
            &sep_d as &dyn IsMenuItem,
            &custom,
            &set_custom,
            &sep_a,
            &away,
        ]);
        let durations = Submenu::with_items("Keep awake for", true, &duration_items)?;

        let sep_s = sep();
        let settings = Submenu::with_items(
            "Settings",
            true,
            &[
                &autostart,
                &display,
                &resume,
                &sep_s,
                &open_config,
                &reload,
                &open_log,
            ],
        )?;

        let menu = Menu::with_items(&[
            &status,
            &sep(),
            &toggle,
            &durations,
            &sep(),
            &emergency,
            &sep(),
            &settings,
            &quit,
        ])?;

        Ok(Menus {
            menu,
            status,
            detail,
            detail_shown: false,
            toggle,
            indefinite,
            presets,
            custom,
            set_custom,
            away,
            emergency,
            autostart,
            display,
            resume,
            open_config,
            reload,
            open_log,
            quit,
        })
    }

    fn set_detail(&mut self, text: &str) {
        if text.is_empty() {
            if self.detail_shown {
                let _ = self.menu.remove(&self.detail);
                self.detail_shown = false;
            }
        } else {
            self.detail.set_text(text);
            if !self.detail_shown {
                self.detail_shown = self.menu.insert(&self.detail, 1).is_ok();
            }
        }
    }
}

fn custom_label(minutes: u32) -> String {
    format!("Custom ({})", describe_minutes(minutes))
}

struct App {
    config: Config,
    config_path: Option<PathBuf>,
    log_path: Option<PathBuf>,
    controller: Controller<SystemInhibitor>,
    lid_support: LidSupport,
    icons: Icons,
    menus: Menus,
    tray: Option<TrayIcon>,
    current_icon: Option<IconKind>,
    hotkeys: Option<GlobalHotKeyManager>,
    hotkey: Option<HotKey>,
    prompt_open: bool,
    proxy: EventLoopProxy<UserEvent>,
    on_battery: bool,
    /// The platform can tell how long the user has been idle.
    idle_supported: bool,
}

pub fn run(config_path: Option<PathBuf>, log_path: Option<PathBuf>) -> ! {
    let config = config_path
        .as_deref()
        .map(Config::load_or_create)
        .unwrap_or_default();

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    forward_events(&proxy);

    let power_info = {
        let p = Mutex::new(proxy.clone());
        let sink: platform::PowerSink = Box::new(move |e| {
            if let Ok(p) = p.lock() {
                let _ = p.send_event(UserEvent::Power(e));
            }
        });
        platform::start_power_monitor(sink).unwrap_or_else(|e| {
            error!("power monitoring unavailable: {e}");
            PowerInfo {
                lid: LidSupport::Unavailable(e.to_string()),
                battery_monitored: false,
            }
        })
    };
    if let LidSupport::Unavailable(why) = &power_info.lid {
        warn!("lid detection unavailable: {why}");
    }
    if !power_info.battery_monitored {
        info!("no battery reported; low-battery pause inactive");
    }
    let lid_support = power_info.lid;

    // Keep the stored autostart entry pointing at this executable.
    if platform::autostart::is_enabled() {
        if let Err(e) = platform::autostart::set_enabled(true) {
            warn!("could not refresh autostart entry: {e}");
        }
    }

    let menus = Menus::build(&config).expect("tray menu");
    let controller = Controller::new(
        SystemInhibitor::new(),
        config.policy(),
        config.inhibit_options(),
    );
    let mut app = App {
        config,
        config_path,
        log_path,
        controller,
        lid_support,
        icons: Icons::new(),
        menus,
        tray: None,
        current_icon: None,
        hotkeys: None,
        hotkey: None,
        prompt_open: false,
        proxy,
        on_battery: false,
        idle_supported: false,
    };

    event_loop.run(move |event, _target, control_flow| {
        match event {
            Event::NewEvents(StartCause::Init) => app.start(),
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => app.tick(),
            Event::UserEvent(e) => {
                if app.handle(e) {
                    app.shutdown();
                    *control_flow = ControlFlow::Exit;
                    return;
                }
            }
            _ => return,
        }
        *control_flow = match app.next_wake() {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        };
    })
}

/// Route tray, menu and hotkey callbacks (which may fire on other threads)
/// into the event loop.
fn forward_events(proxy: &EventLoopProxy<UserEvent>) {
    let p = Mutex::new(proxy.clone());
    TrayIconEvent::set_event_handler(Some(move |e| {
        if let Ok(p) = p.lock() {
            let _ = p.send_event(UserEvent::Tray(e));
        }
    }));
    let p = Mutex::new(proxy.clone());
    MenuEvent::set_event_handler(Some(move |e| {
        if let Ok(p) = p.lock() {
            let _ = p.send_event(UserEvent::Menu(e));
        }
    }));
    let p = Mutex::new(proxy.clone());
    GlobalHotKeyEvent::set_event_handler(Some(move |e| {
        if let Ok(p) = p.lock() {
            let _ = p.send_event(UserEvent::HotKey(e));
        }
    }));
}

impl App {
    fn start(&mut self) {
        let tray = TrayIconBuilder::new()
            .with_id("perk")
            .with_menu(Box::new(self.menus.menu.clone()))
            .with_menu_on_left_click(false)
            .with_tooltip("Perk")
            .with_icon(self.icons.get(IconKind::Off))
            .with_title("")
            .build();
        match tray {
            Ok(t) => self.tray = Some(t),
            Err(e) => {
                error!("could not create tray icon: {e}");
                platform::show_error(
                    "Perk",
                    &format!("Perk could not create its system tray icon.\n\n{e}"),
                );
            }
        }
        self.register_hotkey();
        self.idle_supported = platform::idle_time().is_some();
        if !self.idle_supported {
            info!("idle time unavailable; \"{AWAY_LABEL}\" is off");
            self.menus.away.set_enabled(false);
            self.menus
                .away
                .set_text(format!("{AWAY_LABEL} (not available here)"));
        }
        self.refresh();
        info!("Perk {} started", env!("CARGO_PKG_VERSION"));
    }

    fn shutdown(&mut self) {
        self.controller.dispatch(Input::Disable, SystemTime::now());
        if let (Some(m), Some(h)) = (&self.hotkeys, self.hotkey) {
            let _ = m.unregister(h);
        }
        self.tray = None; // removes the icon immediately on Windows
        info!("Perk exiting");
        log::logger().flush();
    }

    fn register_hotkey(&mut self) {
        if let (Some(m), Some(h)) = (&self.hotkeys, self.hotkey.take()) {
            let _ = m.unregister(h);
        }
        let spec = self.config.emergency_hotkey.trim();
        if spec.is_empty() {
            return;
        }
        let hotkey = match HotKey::from_str(spec) {
            Ok(h) => h,
            Err(e) => {
                warn!("invalid emergency_hotkey {spec:?}: {e}");
                return;
            }
        };
        if self.hotkeys.is_none() {
            match GlobalHotKeyManager::new() {
                Ok(m) => self.hotkeys = Some(m),
                Err(e) => {
                    warn!("global shortcuts unavailable (e.g. on Wayland): {e}");
                    return;
                }
            }
        }
        if let Some(m) = &self.hotkeys {
            match m.register(hotkey) {
                Ok(()) => {
                    info!("emergency stop shortcut: {spec}");
                    self.hotkey = Some(hotkey);
                }
                Err(e) => warn!("could not register {spec}: {e}"),
            }
        }
        let label = match self.hotkey {
            Some(_) => format!("Emergency stop\t{spec}"),
            None => "Emergency stop".into(),
        };
        self.menus.emergency.set_text(label);
    }

    /// Returns true when the app should quit.
    fn handle(&mut self, event: UserEvent) -> bool {
        match event {
            UserEvent::Power(PowerEvent::OnBattery(on)) => {
                info!("power source: {}", if on { "battery" } else { "AC" });
                self.on_battery = on;
                self.input(Input::OnBattery(on));
                // Sleep timeouts differ on battery and AC power.
                self.tick();
            }
            UserEvent::Power(e) => self.input(match e {
                PowerEvent::OnBattery(on) => Input::OnBattery(on),
                PowerEvent::BatteryLevel(p) => {
                    info!("battery: {p}%");
                    Input::BatteryLevel(p)
                }
                PowerEvent::LidClosed => Input::LidClosed,
                PowerEvent::LidOpened => Input::LidOpened,
                PowerEvent::Suspending => Input::Suspending,
                PowerEvent::Resumed => Input::Resumed,
            }),
            UserEvent::Tray(TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }) => self.toggle(),
            UserEvent::Tray(_) => {}
            UserEvent::HotKey(e) => {
                if e.state == HotKeyState::Pressed && self.hotkey.is_some_and(|h| h.id() == e.id) {
                    self.input(Input::EmergencyStop);
                }
            }
            UserEvent::Menu(e) => return self.on_menu(&e.id),
            UserEvent::CustomDuration {
                input,
                dialog_available,
            } => self.on_custom_duration(input, dialog_available),
        }
        false
    }

    fn on_menu(&mut self, id: &MenuId) -> bool {
        let m = &self.menus;
        if id == m.quit.id() {
            return true;
        } else if id == m.toggle.id() {
            self.toggle();
        } else if id == m.emergency.id() {
            self.input(Input::EmergencyStop);
        } else if id == m.indefinite.id() {
            self.start_session(Mode::Indefinite);
        } else if let Some(&(minutes, _)) = m.presets.iter().find(|(_, item)| item.id() == id) {
            self.start_session(Mode::from_minutes(minutes));
        } else if id == m.custom.id() {
            self.start_session(Mode::from_minutes(self.config.custom_minutes));
        } else if id == m.away.id() {
            self.config.start_timer_when_away = !self.config.start_timer_when_away;
            self.save_config();
            // Apply to a running timer straight away.
            if let Some(Mode::For(d)) = self.controller.effective().session().map(|s| s.mode) {
                self.enable(Mode::For(d));
            }
        } else if id == m.set_custom.id() {
            let initial = describe_short(self.config.custom_minutes);
            self.prompt(PROMPT_TEXT, &initial);
        } else if id == m.autostart.id() {
            let want = !platform::autostart::is_enabled();
            if let Err(e) = platform::autostart::set_enabled(want) {
                error!("autostart: {e}");
                platform::show_error(
                    "Perk",
                    &format!("Could not change the startup setting.\n\n{e}"),
                );
            }
        } else if id == m.display.id() {
            self.config.keep_display_on = !self.config.keep_display_on;
            self.save_config();
            self.apply_config();
        } else if id == m.resume.id() {
            self.config.resume_after_lid_open = !self.config.resume_after_lid_open;
            self.save_config();
            self.apply_config();
        } else if id == m.open_config.id() {
            if let Some(path) = &self.config_path {
                if !path.exists() {
                    self.save_config();
                }
                open(path);
            }
        } else if id == m.reload.id() {
            if let Some(path) = &self.config_path {
                self.config = Config::load_or_create(path);
                self.apply_config();
                self.register_hotkey();
                info!("settings reloaded");
            }
        } else if id == m.open_log.id() {
            if let Some(path) = &self.log_path {
                open(path);
            }
        }
        self.refresh();
        false
    }

    fn toggle(&mut self) {
        if self.controller.effective().session().is_some() {
            self.input(Input::Disable);
        } else {
            self.enable(self.config.last_mode());
        }
    }

    fn start_session(&mut self, mode: Mode) {
        if self.config.last_minutes != mode.as_minutes() {
            self.config.last_minutes = mode.as_minutes();
            self.save_config();
        }
        self.enable(mode);
    }

    /// Start a session, honouring "Start timer when I step away".
    fn enable(&mut self, mode: Mode) {
        match mode {
            Mode::For(d) if self.config.start_timer_when_away && self.idle_supported => {
                self.input(Input::EnableWhenAway(d));
                self.tick();
            }
            _ => self.input(Input::Enable(mode)),
        }
    }

    fn input(&mut self, input: Input) {
        self.controller.dispatch(input, SystemTime::now());
        self.refresh();
    }

    /// Re-evaluate timers, with fresh activity information when it matters.
    fn tick(&mut self) {
        let away = self
            .controller
            .effective()
            .session()
            .is_some_and(|s| s.when_away);
        match self.idle_sample().filter(|_| away) {
            Some(sample) => self.input(Input::Idle(sample)),
            None => self.input(Input::Tick),
        }
    }

    fn idle_sample(&self) -> Option<IdleSample> {
        let idle = platform::idle_time()?;
        let timeouts = platform::sleep_timeouts(self.on_battery);
        Some(IdleSample {
            last_input: SystemTime::now() - idle,
            takeover_after: away::takeover_after(timeouts, self.config.keep_display_on),
        })
    }

    fn prompt(&mut self, message: &str, initial: &str) {
        if self.prompt_open {
            return;
        }
        self.prompt_open = true;
        let proxy = Mutex::new(self.proxy.clone());
        platform::prompt_duration(
            PROMPT_TITLE,
            message,
            initial,
            Box::new(move |input, dialog_available| {
                if let Ok(p) = proxy.lock() {
                    let _ = p.send_event(UserEvent::CustomDuration {
                        input,
                        dialog_available,
                    });
                }
            }),
        );
    }

    fn on_custom_duration(&mut self, input: Option<String>, dialog_available: bool) {
        self.prompt_open = false;
        if !dialog_available {
            // No dialog tool (e.g. Linux without zenity/kdialog): edit the file.
            warn!("no input dialog available; opening the settings file instead");
            if let Some(path) = &self.config_path {
                open(path);
            }
            return;
        }
        let Some(text) = input else { return };
        match parse_minutes(&text) {
            Ok(minutes) => {
                self.config.custom_minutes = minutes;
                self.menus.custom.set_text(custom_label(minutes));
                self.start_session(Mode::from_minutes(minutes));
            }
            Err(e) => {
                let message = format!("{e}.\n\n{PROMPT_TEXT}");
                self.prompt(&message, &text);
            }
        }
    }

    fn apply_config(&mut self) {
        self.controller.set_policy(self.config.policy());
        self.controller.set_options(self.config.inhibit_options());
        self.tick();
        self.menus
            .custom
            .set_text(custom_label(self.config.custom_minutes));
    }

    fn save_config(&self) {
        if let Some(path) = &self.config_path {
            if let Err(e) = self.config.save(path) {
                error!("could not save settings: {e}");
            }
        }
    }

    /// Bring icon, tooltip and menu in line with the current state.
    fn refresh(&mut self) {
        let now = SystemTime::now();
        let effective = self.controller.effective();
        let (status, detail) = describe(
            &effective,
            now,
            self.config.resume_after_lid_open,
            self.controller.last_error(),
            &self.lid_support,
        );

        let m = &self.menus;
        m.status.set_text(&status);
        let session = effective.session();
        m.toggle.set_checked(session.is_some());
        let mode = session.map(|s| s.mode);
        m.indefinite.set_checked(mode == Some(Mode::Indefinite));
        let mut preset_hit = false;
        for (minutes, item) in &m.presets {
            let hit = mode == Some(Mode::from_minutes(*minutes));
            preset_hit |= hit;
            item.set_checked(hit);
        }
        m.custom
            .set_checked(matches!(mode, Some(Mode::For(_))) && !preset_hit);
        m.autostart.set_checked(platform::autostart::is_enabled());
        m.display.set_checked(self.config.keep_display_on);
        m.resume.set_checked(self.config.resume_after_lid_open);
        m.away
            .set_checked(self.config.start_timer_when_away && self.idle_supported);
        self.menus.set_detail(&detail);

        let kind = icon::kind_for(&effective);
        if let Some(tray) = &self.tray {
            if self.current_icon != Some(kind) {
                if let Err(e) = tray.set_icon(Some(self.icons.get(kind))) {
                    warn!("could not update tray icon: {e}");
                }
                self.current_icon = Some(kind);
            }
            let tooltip = if detail.is_empty() {
                format!("Perk — {status}")
            } else {
                format!("Perk — {status}\n{detail}")
            };
            let _ = tray.set_tooltip(Some(tooltip));
        }
    }

    /// When to wake up next: the session deadline, the next minute boundary
    /// of the remaining time so the countdown stays current, or the moment a
    /// waiting "when away" session has to step in.
    fn next_wake(&self) -> Option<Instant> {
        let now = SystemTime::now();
        let until = |t: SystemTime| t.duration_since(now).unwrap_or(Duration::ZERO);
        let countdown = self.controller.next_deadline().map(|deadline| {
            let left = until(deadline);
            let to_next_minute = match left.as_millis() % 60_000 {
                0 => Duration::from_secs(60),
                ms => Duration::from_millis(ms as u64),
            };
            left.min(to_next_minute)
        });
        let takeover = self.controller.takeover_at().map(until);
        let wait = match (countdown, takeover) {
            (Some(a), Some(b)) => a.min(b),
            (a, b) => a.or(b)?,
        };
        Some(Instant::now() + wait + Duration::from_millis(20))
    }
}

fn open(path: &std::path::Path) {
    if let Err(e) = platform::open_path(path) {
        warn!("could not open {}: {e}", path.display());
    }
}

/// "45m", "2h", "1h30m": compact form for prefilling the input box.
fn describe_short(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h{m}m"),
    }
}

/// Status line and optional detail line for the menu and tooltip.
fn describe(
    effective: &Effective,
    now: SystemTime,
    resume: bool,
    error: Option<&str>,
    lid: &LidSupport,
) -> (String, String) {
    let left = |s: &crate::core::Session| match s.remaining(now) {
        Some(d) => format!("{} left", format_remaining(d)),
        None => "indefinitely".to_string(),
    };
    let (status, mut detail) = match effective {
        Effective::Disabled => ("Keep-awake is off".to_string(), String::new()),
        Effective::Active(s) if s.when_away => (
            format!("Keeping awake — {}", left(s)),
            "Timer starts over when you're back".to_string(),
        ),
        Effective::Active(s) => (format!("Keeping awake — {}", left(s)), String::new()),
        Effective::Waiting(s) => {
            let full = match s.mode {
                Mode::For(d) => d,
                Mode::Indefinite => Duration::ZERO,
            };
            let rest = s.remaining(now).unwrap_or(Duration::ZERO);
            if rest + LEAD >= full {
                let minutes = u32::try_from(full.as_secs() / 60).unwrap_or(u32::MAX);
                (
                    "Waiting for you to step away".to_string(),
                    format!("Then keeps awake for {}", describe_minutes(minutes)),
                )
            } else {
                (
                    format!("You're away — {} left", format_remaining(rest)),
                    "Steps in just before your computer would sleep".to_string(),
                )
            }
        }
        Effective::Paused(s, reason) => {
            let why = match reason {
                PauseReason::LidClosed => "lid closed",
                PauseReason::Suspended => "system asleep",
                PauseReason::LowBattery => "battery low",
            };
            let next = if *reason == PauseReason::LowBattery {
                format!("Resumes when charging ({})", left(s))
            } else if resume {
                format!("Resumes when reopened ({})", left(s))
            } else {
                "Ends when the lid reopens".to_string()
            };
            (format!("Paused — {why}"), next)
        }
    };
    if let Some(e) = error {
        detail = format!("⚠ Could not keep awake: {e}");
    } else if detail.is_empty() {
        if let LidSupport::Unavailable(_) = lid {
            detail = "⚠ Lid detection unavailable".into();
        }
    }
    (status, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Session;

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    #[test]
    fn describes_states() {
        let lid = LidSupport::Monitored;
        let off = describe(&Effective::Disabled, now(), true, None, &lid);
        assert_eq!(off, ("Keep-awake is off".into(), String::new()));

        let s = Session::start(Mode::For(Duration::from_secs(3900)), now());
        let (status, _) = describe(&Effective::Active(s), now(), true, None, &lid);
        assert_eq!(status, "Keeping awake — 1 h 05 min left");

        let (status, detail) = describe(
            &Effective::Paused(s, PauseReason::LidClosed),
            now(),
            true,
            None,
            &lid,
        );
        assert_eq!(status, "Paused — lid closed");
        assert!(detail.starts_with("Resumes when reopened"));
    }

    #[test]
    fn describes_away_sessions() {
        let lid = LidSupport::Monitored;
        let s = Session::start_when_away(Duration::from_secs(1800), now());
        let (status, detail) = describe(&Effective::Waiting(s), now(), true, None, &lid);
        assert_eq!(status, "Waiting for you to step away");
        assert_eq!(detail, "Then keeps awake for 30 minutes");

        let later = now() + Duration::from_secs(600);
        let (status, _) = describe(&Effective::Waiting(s), later, true, None, &lid);
        assert_eq!(status, "You're away — 20 min left");
    }

    #[test]
    fn error_and_lid_warnings_show_in_detail() {
        let lid = LidSupport::Unavailable("no upower".into());
        let (_, d) = describe(&Effective::Disabled, now(), true, None, &lid);
        assert_eq!(d, "⚠ Lid detection unavailable");
        let (_, d) = describe(&Effective::Disabled, now(), true, Some("denied"), &lid);
        assert_eq!(d, "⚠ Could not keep awake: denied");
    }

    #[test]
    fn short_durations() {
        assert_eq!(describe_short(45), "45m");
        assert_eq!(describe_short(120), "2h");
        assert_eq!(describe_short(90), "1h30m");
        assert_eq!(parse_minutes(&describe_short(90)), Ok(90));
    }

    #[test]
    fn default_hotkey_parses() {
        assert!(HotKey::from_str(&Config::default().emergency_hotkey).is_ok());
    }
}
