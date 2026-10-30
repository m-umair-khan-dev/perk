//! When a "when away" session has to step in, given the OS power settings.

use std::time::Duration;

/// Perk steps in this long before the OS would act on its own.
pub const LEAD: Duration = Duration::from_secs(60);

/// The OS idle timeouts for the current power source. `None` means "never".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub sleep: Option<Duration>,
    pub display: Option<Duration>,
}

/// How long the user can be idle before Perk must hold the OS lock.
///
/// - Sleep after 10 min: step in after 9 min idle.
/// - With "Keep display on", the screen-off timeout counts too, so the screen
///   does not go dark first.
/// - The OS never sleeps (or blanks the screen): `None`, nothing to do.
/// - Settings unknown: step in immediately, the safe choice.
pub fn takeover_after(timeouts: Option<Timeouts>, keep_display_on: bool) -> Option<Duration> {
    let Some(t) = timeouts else {
        return Some(Duration::ZERO);
    };
    let display = t.display.filter(|_| keep_display_on);
    let first = match (t.sleep, display) {
        (Some(a), Some(b)) => a.min(b),
        (a, b) => a.or(b)?,
    };
    Some(first.saturating_sub(LEAD))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    fn t(sleep: Option<u32>, display: Option<u32>) -> Option<Timeouts> {
        Some(Timeouts {
            sleep: sleep.map(|m| m * MIN),
            display: display.map(|m| m * MIN),
        })
    }

    #[test]
    fn steps_in_a_minute_before_sleep() {
        assert_eq!(takeover_after(t(Some(10), None), false), Some(9 * MIN));
        assert_eq!(takeover_after(t(Some(10), Some(5)), false), Some(9 * MIN));
    }

    #[test]
    fn keep_display_on_also_respects_screen_off() {
        assert_eq!(takeover_after(t(Some(10), Some(5)), true), Some(4 * MIN));
        assert_eq!(takeover_after(t(None, Some(5)), true), Some(4 * MIN));
    }

    #[test]
    fn never_sleeping_needs_nothing() {
        assert_eq!(takeover_after(t(None, None), true), None);
        assert_eq!(takeover_after(t(None, Some(5)), false), None);
    }

    #[test]
    fn short_or_unknown_timeouts_step_in_immediately() {
        assert_eq!(
            takeover_after(
                Some(Timeouts {
                    sleep: Some(Duration::from_secs(30)),
                    display: None
                }),
                false
            ),
            Some(Duration::ZERO)
        );
        assert_eq!(takeover_after(None, true), Some(Duration::ZERO));
    }
}
