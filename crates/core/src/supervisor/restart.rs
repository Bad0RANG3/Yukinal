//! 有界重启：策略、退避、退出与重启记录，以及一次退出的纯决策。
//!
//! 这些都是纯数据与纯函数，所以退避与预算能在不派生任何进程的情况下被测试。

use std::time::{Duration, Instant};

use serde::Serialize;

/// How the supervisor reacts to an exit **it did not ask for**.
///
/// Bounded on purpose, and the bound is the point: an agent that dies during startup
/// (a missing `node`, a syntax error in the bundle) would otherwise be respawned
/// forever, burning a process slot and filling the log tail with the same failure. Once
/// the budget is spent the supervisor stops trying and says so
/// ([`RestartRecord::exhausted`]), and the next start has to come from the user.
///
/// `healthy_after` is what keeps the budget from being consumed by an unrelated crash
/// hours later: a process that served longer than this is treated as a fresh baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartPolicy {
    pub enabled: bool,
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub healthy_after: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: 5,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            healthy_after: Duration::from_secs(60),
        }
    }
}

/// Exponential backoff, capped. Attempts are numbered from 1, so the first retry waits
/// `base_delay` rather than twice it.
#[must_use]
pub fn restart_delay(policy: &RestartPolicy, attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    policy
        .base_delay
        .saturating_mul(2u32.saturating_pow(exponent))
        .min(policy.max_delay)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitRecord {
    pub code: Option<i32>,
    pub signal: Option<String>,
    pub at: String,
}

/// The supervisor's own account of an automatic restart, for the UI to render instead of
/// leaving a crash and a restart looking like nothing happened.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartRecord {
    /// The attempt this record describes: 1 for the first retry.
    pub attempt: u32,
    pub max_attempts: u32,
    /// True once the budget is spent and the supervisor has stopped trying.
    pub exhausted: bool,
    pub at: String,
}

/// What the supervisor decided about one unexpected exit. Pure data so the backoff and the
/// budget can be tested without spawning a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RestartDecision {
    Retry { attempt: u32, delay: Duration },
    Exhausted { attempts: u32 },
}

#[derive(Debug, Default)]
pub(super) struct RestartState {
    /// Attempts spent since the last healthy start.
    attempts: u32,
    /// When the current process started; `None` while nothing is running. Cleared on exit
    /// so a second decision inside the same outage cannot reset the budget a second time.
    pub(super) started_at: Option<Instant>,
    pub(super) record: Option<RestartRecord>,
}

impl RestartState {
    pub(super) fn decide(
        &mut self,
        policy: &RestartPolicy,
        now: Instant,
        at: String,
    ) -> RestartDecision {
        let was_healthy = self
            .started_at
            .is_some_and(|started| now.duration_since(started) >= policy.healthy_after);
        self.started_at = None;
        if was_healthy {
            self.attempts = 0;
        }

        if !policy.enabled || self.attempts >= policy.max_attempts {
            self.record = Some(RestartRecord {
                attempt: self.attempts,
                max_attempts: policy.max_attempts,
                exhausted: true,
                at,
            });
            return RestartDecision::Exhausted {
                attempts: self.attempts,
            };
        }

        self.attempts += 1;
        let delay = restart_delay(policy, self.attempts);
        self.record = Some(RestartRecord {
            attempt: self.attempts,
            max_attempts: policy.max_attempts,
            exhausted: false,
            at,
        });
        RestartDecision::Retry {
            attempt: self.attempts,
            delay,
        }
    }

    pub(super) fn reset(&mut self) {
        self.attempts = 0;
        self.started_at = None;
        self.record = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RestartPolicy {
        RestartPolicy {
            enabled: true,
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(400),
            healthy_after: Duration::from_secs(60),
        }
    }

    fn at(seconds: u64) -> String {
        format!("2026-01-01T00:00:{seconds:02}Z")
    }

    #[test]
    fn backoff_doubles_from_the_base_and_stops_at_the_cap() {
        let policy = policy();
        assert_eq!(restart_delay(&policy, 1), Duration::from_millis(100));
        assert_eq!(restart_delay(&policy, 2), Duration::from_millis(200));
        assert_eq!(restart_delay(&policy, 3), Duration::from_millis(400));
        assert_eq!(
            restart_delay(&policy, 4),
            Duration::from_millis(400),
            "attempt 4 would be 800 ms; the cap is what the agent actually waits"
        );
        // A huge attempt must not overflow into a panic or a nonsense delay.
        assert_eq!(restart_delay(&policy, u32::MAX), Duration::from_millis(400));
    }

    #[test]
    fn the_budget_is_spent_and_then_the_supervisor_stops() {
        let policy = policy();
        let start = Instant::now();
        let mut state = RestartState {
            attempts: 0,
            started_at: Some(start),
            record: None,
        };

        for expected in 1u32..=3 {
            let decision = state.decide(
                &policy,
                start + Duration::from_secs(1),
                at(u64::from(expected)),
            );
            assert_eq!(
                decision,
                RestartDecision::Retry {
                    attempt: expected,
                    delay: restart_delay(&policy, expected),
                }
            );
        }

        let decision = state.decide(&policy, start + Duration::from_secs(2), at(4));
        assert_eq!(decision, RestartDecision::Exhausted { attempts: 3 });
        let record = state.record.as_ref().expect("a record must be kept");
        assert!(record.exhausted);
        assert_eq!(record.attempt, 3);
        assert_eq!(record.max_attempts, 3);
    }

    #[test]
    fn a_disabled_policy_never_retries_even_with_budget_left() {
        let policy = RestartPolicy {
            enabled: false,
            ..policy()
        };
        let mut state = RestartState {
            attempts: 0,
            started_at: Some(Instant::now()),
            record: None,
        };
        assert_eq!(
            state.decide(&policy, Instant::now(), at(1)),
            RestartDecision::Exhausted { attempts: 0 }
        );
    }

    #[test]
    fn a_process_that_stayed_up_counts_as_a_fresh_baseline() {
        let policy = policy();
        let start = Instant::now();
        let mut state = RestartState {
            attempts: 3,
            started_at: Some(start),
            record: None,
        };

        // Crash after two minutes of service: the budget of a long dead incident must not
        // be charged to this one.
        let decision = state.decide(&policy, start + Duration::from_secs(120), at(9));
        assert_eq!(
            decision,
            RestartDecision::Retry {
                attempt: 1,
                delay: Duration::from_millis(100),
            }
        );
    }

    #[test]
    fn a_second_decision_in_one_outage_cannot_reset_the_budget_twice() {
        let policy = policy();
        let start = Instant::now();
        let mut state = RestartState {
            attempts: 0,
            started_at: Some(start),
            record: None,
        };

        // The crash of a long-running process...
        assert_eq!(
            state.decide(&policy, start + Duration::from_secs(120), at(1)),
            RestartDecision::Retry {
                attempt: 1,
                delay: Duration::from_millis(100),
            }
        );
        // ...and then the *restart failing* must spend the budget rather than see the same
        // ancient `started_at` and hand out a fresh attempt. This is the loop that would
        // otherwise retry forever.
        assert_eq!(
            state.decide(&policy, start + Duration::from_secs(121), at(2)),
            RestartDecision::Retry {
                attempt: 2,
                delay: Duration::from_millis(200),
            }
        );
        assert_eq!(
            state.decide(&policy, start + Duration::from_secs(122), at(3)),
            RestartDecision::Retry {
                attempt: 3,
                delay: Duration::from_millis(400),
            }
        );
        assert_eq!(
            state.decide(&policy, start + Duration::from_secs(123), at(4)),
            RestartDecision::Exhausted { attempts: 3 }
        );
    }

    #[test]
    fn reset_clears_both_the_budget_and_the_record() {
        let policy = policy();
        let start = Instant::now();
        let mut state = RestartState {
            attempts: 2,
            started_at: Some(start),
            record: None,
        };
        state.decide(&policy, start + Duration::from_secs(1), at(1));
        assert!(state.record.is_some());

        state.reset();

        assert_eq!(state.attempts, 0);
        assert!(state.started_at.is_none());
        assert!(state.record.is_none(), "a user stop is not an outage");
    }
}
