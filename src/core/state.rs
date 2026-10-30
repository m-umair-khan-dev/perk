//! Pure keep-awake state machine. No OS calls, no clocks: the current time is
//! always passed in, which keeps every transition deterministic and testable.
//!
//! Safety rules enforced here: while the lid is closed, the system is
//! suspending, or the battery is critically low (on battery power), the
//! effective state is never `Active`, no matter what the user asked for.
//!
//! "When away" sessions: the countdown runs only while the user is idle (it
//! restarts from full whenever they use the computer again), and the OS lock is
//! only taken once the user has been idle long enough that the computer would
//! otherwise go to sleep soon (see [`IdleSample::takeover_after`]).

use std::time::{Duration, SystemTime};

/// How long the user asked to stay awake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Indefinite,
    For(Duration),
}

impl Mode {
    pub fn from_minutes(minutes: u32) -> Self {
        if minutes == 0 {
            Mode::Indefinite
        } else {
            Mode::For(Duration::from_secs(u64::from(minutes) * 60))
        }
    }

    /// Minutes, with 0 meaning indefinitely (the config file encoding).
    pub fn as_minutes(self) -> u32 {
        match self {
            Mode::Indefinite => 0,
            Mode::For(d) => u32::try_from(d.as_secs() / 60).unwrap_or(u32::MAX),
        }
    }
}

/// A keep-awake request the user made at a specific point in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    pub mode: Mode,
    /// Wall-clock end. Wall-clock (not monotonic) time is used on purpose so a
    /// 1-hour session that spends 3 hours suspended has expired on wake.
    /// For a "when away" session it is the last user activity plus the duration.
    pub deadline: Option<SystemTime>,
    /// The timer only runs while the user is away from the computer.
    pub when_away: bool,
}

impl Session {
    pub fn start(mode: Mode, now: SystemTime) -> Self {
        let deadline = match mode {
            Mode::Indefinite => None,
            Mode::For(d) => Some(now + d),
        };
        Session {
            mode,
            deadline,
            when_away: false,
        }
    }

    /// A timed session whose countdown starts once the user is idle.
    pub fn start_when_away(duration: Duration, now: SystemTime) -> Self {
        Session {
            when_away: true,
            ..Session::start(Mode::For(duration), now)
        }
    }

    /// Time left, or `None` for an indefinite session.
    pub fn remaining(&self, now: SystemTime) -> Option<Duration> {
        self.deadline
            .map(|end| end.duration_since(now).unwrap_or(Duration::ZERO))
    }

    pub fn is_expired(&self, now: SystemTime) -> bool {
        self.deadline.is_some_and(|end| now >= end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    LidClosed,
    Suspended,
    /// Running on battery at or below the configured threshold.
    LowBattery,
}

/// What the rest of the app should show and enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effective {
    Disabled,
    Active(Session),
    Paused(Session, PauseReason),
    /// A "when away" session that does not need to hold the OS lock yet: the
    /// user is at the computer, or the OS would not sleep for a while anyway.
    Waiting(Session),
}

impl Effective {
    pub fn wants_inhibition(&self) -> bool {
        matches!(self, Effective::Active(_))
    }

    pub fn session(&self) -> Option<Session> {
        match *self {
            Effective::Disabled => None,
            Effective::Active(s) | Effective::Paused(s, _) | Effective::Waiting(s) => Some(s),
        }
    }
}

/// What the platform reports about user activity, sampled by the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleSample {
    /// When the user last touched the keyboard or mouse.
    pub last_input: SystemTime,
    /// How long the user must be idle before Perk has to step in (just before
    /// the OS would sleep). `None`: the OS never sleeps on idle, so Perk never
    /// needs to step in.
    pub takeover_after: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Enable(Mode),
    /// Keep awake for this long, counted only while the user is away.
    EnableWhenAway(Duration),
    /// Fresh user-activity information.
    Idle(IdleSample),
    Disable,
    EmergencyStop,
    LidClosed,
    LidOpened,
    Suspending,
    Resumed,
    /// Power source changed: `true` when running on battery.
    OnBattery(bool),
    /// Remaining battery charge, 0-100.
    BatteryLevel(u8),
    /// Re-evaluate timers (sent when a deadline may have passed).
    Tick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Continue the previous session after the lid reopens / the system wakes.
    /// When false, closing the lid or suspending ends the session.
    pub resume_after_pause: bool,
    /// Pause while on battery at or below this charge (percent); 0 disables.
    pub low_battery_percent: u8,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            resume_after_pause: true,
            low_battery_percent: 10,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct KeepAwake {
    session: Option<Session>,
    /// Latest known user activity (enabling a session counts as activity).
    active_at: Option<SystemTime>,
    idle: Option<IdleSample>,
    /// A "when away" session currently needs the OS lock.
    away_engaged: bool,
    lid_closed: bool,
    suspended: bool,
    on_battery: bool,
    battery_percent: Option<u8>,
    battery_low: bool,
}

impl KeepAwake {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lid_closed(&self) -> bool {
        self.lid_closed
    }

    pub fn effective(&self) -> Effective {
        match self.session {
            None => Effective::Disabled,
            Some(s) if self.suspended => Effective::Paused(s, PauseReason::Suspended),
            Some(s) if self.lid_closed => Effective::Paused(s, PauseReason::LidClosed),
            Some(s) if self.battery_low => Effective::Paused(s, PauseReason::LowBattery),
            Some(s) if s.when_away && !self.away_engaged => Effective::Waiting(s),
            Some(s) => Effective::Active(s),
        }
    }

    /// Next instant at which the state changes on its own (session expiry).
    pub fn next_deadline(&self) -> Option<SystemTime> {
        self.session.and_then(|s| s.deadline)
    }

    /// When a waiting "when away" session will need the OS lock, assuming the
    /// user stays idle until then.
    pub fn takeover_at(&self) -> Option<SystemTime> {
        let s = self.session?;
        if !s.when_away || self.away_engaged {
            return None;
        }
        let after = self.idle?.takeover_after?;
        Some(self.active_at? + after)
    }

    pub fn handle(&mut self, input: Input, now: SystemTime, policy: Policy) -> Effective {
        // New activity first: a user who just came back must not see the
        // session expire on stale information.
        if let Input::Idle(sample) = input {
            self.idle = Some(sample);
            self.note_activity(sample.last_input);
            self.sync_away(now);
        }
        self.expire(now);
        match input {
            Input::Enable(mode) => {
                self.session = Some(Session::start(mode, now));
                self.note_activity(now);
            }
            Input::EnableWhenAway(d) => {
                self.session = Some(Session::start_when_away(d, now));
                self.note_activity(now);
            }
            Input::Idle(_) => {}
            Input::Disable | Input::EmergencyStop => self.session = None,
            Input::LidClosed => self.lid_closed = true,
            Input::LidOpened => {
                // Only a real closed -> open edge counts; Windows reports the
                // current (open) state on registration, which must be a no-op.
                if std::mem::replace(&mut self.lid_closed, false) && !policy.resume_after_pause {
                    self.session = None;
                }
            }
            Input::Suspending => self.suspended = true,
            Input::Resumed => {
                if std::mem::replace(&mut self.suspended, false) && !policy.resume_after_pause {
                    self.session = None;
                }
            }
            Input::OnBattery(on) => self.on_battery = on,
            Input::BatteryLevel(p) => self.battery_percent = Some(p.min(100)),
            Input::Tick => {}
        }
        // Battery is a level, not an edge: re-derive it every time so policy
        // changes take effect too. Plugging in resumes automatically.
        self.battery_low = policy.low_battery_percent > 0
            && self.on_battery
            && self
                .battery_percent
                .is_some_and(|p| p <= policy.low_battery_percent);
        self.sync_away(now);
        self.effective()
    }

    fn note_activity(&mut self, at: SystemTime) {
        self.active_at = Some(self.active_at.map_or(at, |prev| prev.max(at)));
    }

    /// Re-derive the deadline of a "when away" session from the latest
    /// activity, and whether it needs the OS lock right now.
    fn sync_away(&mut self, now: SystemTime) {
        let Some(s) = self.session.as_mut().filter(|s| s.when_away) else {
            self.away_engaged = false;
            return;
        };
        let (Mode::For(d), Some(active_at)) = (s.mode, self.active_at) else {
            return;
        };
        s.deadline = Some(active_at + d);
        let idle_for = now.duration_since(active_at).unwrap_or(Duration::ZERO);
        self.away_engaged = match self.idle {
            // No activity information at all: behave like a normal timer.
            None => true,
            Some(sample) => sample.takeover_after.is_some_and(|t| idle_for >= t),
        };
    }

    fn expire(&mut self, now: SystemTime) {
        if self.session.is_some_and(|s| s.is_expired(now)) {
            self.session = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn resume() -> Policy {
        Policy::default()
    }

    fn no_resume() -> Policy {
        Policy {
            resume_after_pause: false,
            ..Policy::default()
        }
    }

    #[test]
    fn starts_disabled() {
        assert_eq!(KeepAwake::new().effective(), Effective::Disabled);
    }

    #[test]
    fn enable_indefinitely_never_expires() {
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::Indefinite), t0(), resume());
        let later = t0() + Duration::from_secs(365 * 24 * 3600);
        assert!(k.handle(Input::Tick, later, resume()).wants_inhibition());
        assert_eq!(k.next_deadline(), None);
    }

    #[test]
    fn timed_session_expires_exactly_at_deadline() {
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::For(30 * MIN)), t0(), resume());
        let s = k.effective().session().unwrap();
        assert_eq!(s.remaining(t0() + 10 * MIN), Some(20 * MIN));
        assert!(
            k.handle(
                Input::Tick,
                t0() + 30 * MIN - Duration::from_secs(1),
                resume()
            )
            .wants_inhibition()
        );
        assert_eq!(
            k.handle(Input::Tick, t0() + 30 * MIN, resume()),
            Effective::Disabled
        );
    }

    #[test]
    fn re_enabling_restarts_the_timer() {
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::For(30 * MIN)), t0(), resume());
        k.handle(
            Input::Enable(Mode::For(60 * MIN)),
            t0() + 20 * MIN,
            resume(),
        );
        assert_eq!(k.next_deadline(), Some(t0() + 80 * MIN));
    }

    #[test]
    fn disable_and_emergency_stop_end_session() {
        for stop in [Input::Disable, Input::EmergencyStop] {
            let mut k = KeepAwake::new();
            k.handle(Input::Enable(Mode::Indefinite), t0(), resume());
            assert_eq!(k.handle(stop, t0(), resume()), Effective::Disabled);
        }
    }

    #[test]
    fn lid_close_pauses_immediately() {
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::Indefinite), t0(), resume());
        let e = k.handle(Input::LidClosed, t0(), resume());
        assert!(!e.wants_inhibition());
        assert!(matches!(e, Effective::Paused(_, PauseReason::LidClosed)));
    }

    #[test]
    fn lid_open_resumes_when_policy_allows() {
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::Indefinite), t0(), resume());
        k.handle(Input::LidClosed, t0(), resume());
        assert!(
            k.handle(Input::LidOpened, t0() + MIN, resume())
                .wants_inhibition()
        );
    }

    #[test]
    fn lid_open_ends_session_when_policy_forbids_resume() {
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::Indefinite), t0(), no_resume());
        k.handle(Input::LidClosed, t0(), no_resume());
        assert_eq!(
            k.handle(Input::LidOpened, t0(), no_resume()),
            Effective::Disabled
        );
    }

    #[test]
    fn spurious_lid_open_does_not_end_session() {
        // Windows sends the current lid state ("open") right after registering.
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::Indefinite), t0(), no_resume());
        assert!(
            k.handle(Input::LidOpened, t0(), no_resume())
                .wants_inhibition()
        );
    }

    #[test]
    fn enabling_while_lid_closed_stays_paused() {
        let mut k = KeepAwake::new();
        k.handle(Input::LidClosed, t0(), resume());
        let e = k.handle(Input::Enable(Mode::Indefinite), t0(), resume());
        assert!(matches!(e, Effective::Paused(_, PauseReason::LidClosed)));
        assert!(!e.wants_inhibition());
    }

    #[test]
    fn timer_keeps_running_while_paused() {
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::For(60 * MIN)), t0(), resume());
        k.handle(Input::LidClosed, t0() + 10 * MIN, resume());
        // Lid reopened 2 hours later: the 1-hour session is over.
        assert_eq!(
            k.handle(Input::LidOpened, t0() + 130 * MIN, resume()),
            Effective::Disabled
        );
    }

    #[test]
    fn suspend_pauses_and_resume_restores() {
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::Indefinite), t0(), resume());
        let e = k.handle(Input::Suspending, t0(), resume());
        assert!(matches!(e, Effective::Paused(_, PauseReason::Suspended)));
        assert!(k.handle(Input::Resumed, t0(), resume()).wants_inhibition());
    }

    #[test]
    fn resume_with_lid_still_closed_stays_paused() {
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::Indefinite), t0(), resume());
        k.handle(Input::LidClosed, t0(), resume());
        k.handle(Input::Suspending, t0(), resume());
        let e = k.handle(Input::Resumed, t0(), resume());
        assert!(matches!(e, Effective::Paused(_, PauseReason::LidClosed)));
    }

    #[test]
    fn disable_while_paused_prevents_resume() {
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::Indefinite), t0(), resume());
        k.handle(Input::LidClosed, t0(), resume());
        k.handle(Input::EmergencyStop, t0(), resume());
        assert_eq!(
            k.handle(Input::LidOpened, t0(), resume()),
            Effective::Disabled
        );
    }

    #[test]
    fn low_battery_on_battery_pauses_and_charging_resumes() {
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::Indefinite), t0(), resume());
        k.handle(Input::OnBattery(true), t0(), resume());
        assert!(
            k.handle(Input::BatteryLevel(11), t0(), resume())
                .wants_inhibition()
        );
        let e = k.handle(Input::BatteryLevel(10), t0(), resume());
        assert!(matches!(e, Effective::Paused(_, PauseReason::LowBattery)));
        assert!(
            k.handle(Input::OnBattery(false), t0(), resume())
                .wants_inhibition()
        );
    }

    #[test]
    fn low_battery_on_ac_power_does_not_pause() {
        let mut k = KeepAwake::new();
        k.handle(Input::BatteryLevel(3), t0(), resume());
        k.handle(Input::OnBattery(false), t0(), resume());
        assert!(
            k.handle(Input::Enable(Mode::Indefinite), t0(), resume())
                .wants_inhibition()
        );
    }

    #[test]
    fn enabling_on_low_battery_stays_paused() {
        let mut k = KeepAwake::new();
        k.handle(Input::OnBattery(true), t0(), resume());
        k.handle(Input::BatteryLevel(5), t0(), resume());
        let e = k.handle(Input::Enable(Mode::Indefinite), t0(), resume());
        assert!(matches!(e, Effective::Paused(_, PauseReason::LowBattery)));
    }

    #[test]
    fn zero_threshold_disables_battery_protection() {
        let off = Policy {
            low_battery_percent: 0,
            ..Policy::default()
        };
        let mut k = KeepAwake::new();
        k.handle(Input::OnBattery(true), t0(), off);
        k.handle(Input::BatteryLevel(1), t0(), off);
        assert!(
            k.handle(Input::Enable(Mode::Indefinite), t0(), off)
                .wants_inhibition()
        );
    }

    #[test]
    fn lid_takes_priority_over_low_battery() {
        let mut k = KeepAwake::new();
        k.handle(Input::Enable(Mode::Indefinite), t0(), resume());
        k.handle(Input::OnBattery(true), t0(), resume());
        k.handle(Input::BatteryLevel(5), t0(), resume());
        let e = k.handle(Input::LidClosed, t0(), resume());
        assert!(matches!(e, Effective::Paused(_, PauseReason::LidClosed)));
    }

    fn idle(last_input: SystemTime, takeover_min: Option<u32>) -> Input {
        Input::Idle(IdleSample {
            last_input,
            takeover_after: takeover_min.map(|m| m * MIN),
        })
    }

    #[test]
    fn away_session_waits_while_user_is_active() {
        let mut k = KeepAwake::new();
        k.handle(idle(t0(), Some(9)), t0(), resume());
        let e = k.handle(Input::EnableWhenAway(30 * MIN), t0(), resume());
        assert!(matches!(e, Effective::Waiting(_)));
        assert!(!e.wants_inhibition());
        // Two hours of work: the timer keeps restarting and never expires.
        let later = t0() + 120 * MIN;
        let e = k.handle(idle(later, Some(9)), later, resume());
        assert!(matches!(e, Effective::Waiting(_)));
        assert_eq!(k.next_deadline(), Some(later + 30 * MIN));
        assert_eq!(k.takeover_at(), Some(later + 9 * MIN));
    }

    #[test]
    fn away_session_steps_in_just_before_sleep() {
        let mut k = KeepAwake::new();
        k.handle(idle(t0(), Some(9)), t0(), resume());
        k.handle(Input::EnableWhenAway(30 * MIN), t0(), resume());
        // User left at t0; after 8 idle minutes there is nothing to do yet.
        let e = k.handle(idle(t0(), Some(9)), t0() + 8 * MIN, resume());
        assert!(matches!(e, Effective::Waiting(_)));
        // At 9 idle minutes Perk holds the lock.
        let e = k.handle(Input::Tick, t0() + 9 * MIN, resume());
        assert!(e.wants_inhibition());
        assert_eq!(k.takeover_at(), None);
        // The countdown started when the user left.
        let s = e.session().unwrap();
        assert_eq!(s.remaining(t0() + 9 * MIN), Some(21 * MIN));
        // 30 idle minutes: done, the computer may sleep.
        assert_eq!(
            k.handle(Input::Tick, t0() + 30 * MIN, resume()),
            Effective::Disabled
        );
    }

    #[test]
    fn returning_user_releases_and_restarts_the_timer() {
        let mut k = KeepAwake::new();
        k.handle(idle(t0(), Some(9)), t0(), resume());
        k.handle(Input::EnableWhenAway(30 * MIN), t0(), resume());
        assert!(
            k.handle(Input::Tick, t0() + 20 * MIN, resume())
                .wants_inhibition()
        );
        // Back at the keyboard just before the deadline.
        let back = t0() + 29 * MIN;
        let e = k.handle(idle(back, Some(9)), t0() + 30 * MIN, resume());
        assert!(matches!(e, Effective::Waiting(_)));
        assert_eq!(k.next_deadline(), Some(back + 30 * MIN));
    }

    #[test]
    fn away_session_never_steps_in_when_os_never_sleeps() {
        let mut k = KeepAwake::new();
        k.handle(idle(t0(), None), t0(), resume());
        k.handle(Input::EnableWhenAway(30 * MIN), t0(), resume());
        let e = k.handle(Input::Tick, t0() + 29 * MIN, resume());
        assert!(matches!(e, Effective::Waiting(_)));
        assert_eq!(k.takeover_at(), None);
        assert_eq!(
            k.handle(Input::Tick, t0() + 30 * MIN, resume()),
            Effective::Disabled
        );
    }

    #[test]
    fn away_session_without_idle_info_is_a_normal_timer() {
        let mut k = KeepAwake::new();
        let e = k.handle(Input::EnableWhenAway(30 * MIN), t0(), resume());
        assert!(e.wants_inhibition());
        assert_eq!(k.next_deadline(), Some(t0() + 30 * MIN));
    }

    #[test]
    fn lid_close_pauses_an_engaged_away_session() {
        let mut k = KeepAwake::new();
        k.handle(idle(t0(), Some(9)), t0(), resume());
        k.handle(Input::EnableWhenAway(30 * MIN), t0(), resume());
        k.handle(Input::Tick, t0() + 10 * MIN, resume());
        let e = k.handle(Input::LidClosed, t0() + 11 * MIN, resume());
        assert!(matches!(e, Effective::Paused(_, PauseReason::LidClosed)));
        assert!(!e.wants_inhibition());
    }

    #[test]
    fn activity_before_enabling_does_not_shorten_the_timer() {
        let mut k = KeepAwake::new();
        k.handle(Input::EnableWhenAway(30 * MIN), t0() + 5 * MIN, resume());
        // Last input before the session was enabled: enabling counts as activity.
        k.handle(idle(t0(), Some(9)), t0() + 5 * MIN, resume());
        assert_eq!(k.next_deadline(), Some(t0() + 35 * MIN));
    }

    #[test]
    fn mode_minutes_round_trip() {
        assert_eq!(Mode::from_minutes(0), Mode::Indefinite);
        assert_eq!(Mode::from_minutes(90).as_minutes(), 90);
        assert_eq!(Mode::Indefinite.as_minutes(), 0);
    }
}
