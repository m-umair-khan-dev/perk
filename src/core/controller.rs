//! Couples the state machine to an OS inhibitor and keeps them in sync.

use std::time::SystemTime;

use log::{error, info, warn};

use super::state::{Effective, Input, KeepAwake, Policy};
use crate::platform::{InhibitOptions, Inhibitor};

pub struct Controller<I: Inhibitor> {
    state: KeepAwake,
    inhibitor: I,
    policy: Policy,
    options: InhibitOptions,
    last_error: Option<String>,
}

impl<I: Inhibitor> Controller<I> {
    pub fn new(inhibitor: I, policy: Policy, options: InhibitOptions) -> Self {
        Controller {
            state: KeepAwake::new(),
            inhibitor,
            policy,
            options,
            last_error: None,
        }
    }

    pub fn effective(&self) -> Effective {
        self.state.effective()
    }

    pub fn next_deadline(&self) -> Option<SystemTime> {
        self.state.next_deadline()
    }

    pub fn takeover_at(&self) -> Option<SystemTime> {
        self.state.takeover_at()
    }

    pub fn lid_closed(&self) -> bool {
        self.state.lid_closed()
    }

    /// The last failure to acquire an inhibition, cleared on the next success.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn inhibitor(&self) -> &I {
        &self.inhibitor
    }

    /// Change the policy and re-evaluate (e.g. a new battery threshold).
    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
        self.dispatch(Input::Tick, SystemTime::now());
    }

    /// Change inhibition options; re-acquires if currently held so the new
    /// options take effect immediately.
    pub fn set_options(&mut self, options: InhibitOptions) {
        if self.options == options {
            return;
        }
        self.options = options;
        if self.inhibitor.is_held() {
            self.inhibitor.release();
            self.reconcile();
        }
    }

    pub fn dispatch(&mut self, input: Input, now: SystemTime) -> Effective {
        let before = self.state.effective();
        let after = self.state.handle(input, now, self.policy);
        // Activity samples move a waiting session's deadline every time; only
        // log real state changes for them.
        let changed = if matches!(input, Input::Idle(_)) {
            std::mem::discriminant(&before) != std::mem::discriminant(&after)
        } else {
            before != after
        };
        if changed {
            info!("{input:?}: {before:?} -> {after:?}");
        }
        if input == Input::EmergencyStop {
            // Release unconditionally, even if we believe nothing is held.
            warn!("emergency stop: releasing all keep-awake inhibitions");
            self.inhibitor.release();
        }
        self.reconcile();
        self.state.effective()
    }

    fn reconcile(&mut self) {
        let want = self.state.effective().wants_inhibition();
        match (want, self.inhibitor.is_held()) {
            (true, false) => match self.inhibitor.acquire(&self.options) {
                Ok(()) => self.last_error = None,
                Err(e) => {
                    error!("could not keep the system awake: {e}");
                    self.last_error = Some(e.to_string());
                    // Never pretend to be active when we are not.
                    self.state
                        .handle(Input::Disable, SystemTime::now(), self.policy);
                }
            },
            (false, true) => self.inhibitor.release(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::state::{Mode, PauseReason};
    use crate::platform::PlatformError;
    use std::time::Duration;

    #[derive(Default)]
    struct Fake {
        held: bool,
        fail: bool,
        acquires: u32,
        releases: u32,
        last_opts: Option<InhibitOptions>,
    }

    impl Inhibitor for Fake {
        fn acquire(&mut self, opts: &InhibitOptions) -> Result<(), PlatformError> {
            if self.fail {
                return Err(PlatformError::new("denied"));
            }
            self.held = true;
            self.acquires += 1;
            self.last_opts = Some(*opts);
            Ok(())
        }
        fn release(&mut self) {
            self.held = false;
            self.releases += 1;
        }
        fn is_held(&self) -> bool {
            self.held
        }
    }

    fn ctl() -> Controller<Fake> {
        Controller::new(
            Fake::default(),
            Policy::default(),
            InhibitOptions::default(),
        )
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    #[test]
    fn enable_acquires_and_disable_releases() {
        let mut c = ctl();
        c.dispatch(Input::Enable(Mode::Indefinite), now());
        assert!(c.inhibitor().held);
        c.dispatch(Input::Disable, now());
        assert!(!c.inhibitor().held);
    }

    #[test]
    fn lid_close_releases_and_lid_open_reacquires() {
        let mut c = ctl();
        c.dispatch(Input::Enable(Mode::Indefinite), now());
        let e = c.dispatch(Input::LidClosed, now());
        assert!(
            !c.inhibitor().held,
            "inhibition must be released on lid close"
        );
        assert!(matches!(e, Effective::Paused(_, PauseReason::LidClosed)));
        c.dispatch(Input::LidOpened, now());
        assert!(c.inhibitor().held);
        assert_eq!(c.inhibitor().acquires, 2);
    }

    #[test]
    fn suspend_releases() {
        let mut c = ctl();
        c.dispatch(Input::Enable(Mode::Indefinite), now());
        c.dispatch(Input::Suspending, now());
        assert!(!c.inhibitor().held);
    }

    #[test]
    fn emergency_stop_always_calls_release() {
        let mut c = ctl();
        c.dispatch(Input::EmergencyStop, now());
        assert_eq!(
            c.inhibitor().releases,
            1,
            "release even when nothing is held"
        );
        assert_eq!(c.effective(), Effective::Disabled);
    }

    #[test]
    fn acquire_failure_reports_and_disables() {
        let mut c = ctl();
        c.inhibitor.fail = true;
        let e = c.dispatch(Input::Enable(Mode::Indefinite), now());
        assert_eq!(e, Effective::Disabled);
        assert_eq!(c.last_error(), Some("denied"));
        c.inhibitor.fail = false;
        c.dispatch(Input::Enable(Mode::Indefinite), now());
        assert_eq!(c.last_error(), None);
    }

    #[test]
    fn expiry_releases() {
        let mut c = ctl();
        c.dispatch(Input::Enable(Mode::For(Duration::from_secs(60))), now());
        c.dispatch(Input::Tick, now() + Duration::from_secs(61));
        assert!(!c.inhibitor().held);
    }

    #[test]
    fn changing_options_reacquires_with_new_options() {
        let mut c = ctl();
        c.dispatch(Input::Enable(Mode::Indefinite), now());
        let opts = InhibitOptions {
            keep_display_on: false,
        };
        c.set_options(opts);
        assert!(c.inhibitor().held);
        assert_eq!(c.inhibitor().last_opts, Some(opts));
    }

    #[test]
    fn low_battery_releases_and_policy_change_reacquires() {
        let mut c = ctl();
        c.dispatch(Input::Enable(Mode::Indefinite), now());
        c.dispatch(Input::OnBattery(true), now());
        c.dispatch(Input::BatteryLevel(8), now());
        assert!(!c.inhibitor().held);
        c.set_policy(Policy {
            low_battery_percent: 5,
            ..Policy::default()
        });
        assert!(c.inhibitor().held);
    }

    #[test]
    fn away_session_acquires_only_once_idle_long_enough() {
        use crate::core::IdleSample;
        let min = Duration::from_secs(60);
        let sample = |last_input| {
            Input::Idle(IdleSample {
                last_input,
                takeover_after: Some(9 * min),
            })
        };
        let mut c = ctl();
        c.dispatch(sample(now()), now());
        c.dispatch(Input::EnableWhenAway(30 * min), now());
        assert!(!c.inhibitor().held);
        c.dispatch(sample(now()), now() + 9 * min);
        assert!(c.inhibitor().held);
        // The user comes back: release.
        c.dispatch(sample(now() + 12 * min), now() + 12 * min);
        assert!(!c.inhibitor().held);
    }

    #[test]
    fn changing_options_while_disabled_does_not_acquire() {
        let mut c = ctl();
        c.set_options(InhibitOptions {
            keep_display_on: false,
        });
        assert_eq!(c.inhibitor().acquires, 0);
    }
}
