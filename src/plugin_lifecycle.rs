use std::error::Error;
use std::fmt;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};

use crate::plugin_restart_policy::{PluginRestartDecision, PluginRestartPolicy};

const MAX_RECORDED_ERROR_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginDesiredState {
    Disabled,
    Enabled,
}

impl PluginDesiredState {
    pub(crate) fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginOperationalState {
    Unavailable,
    Disabled,
    Stopped,
    Starting,
    Running,
    #[allow(dead_code)] // Reserved for non-ambiguous health signals that do not risk state drift.
    Degraded,
    Stopping,
    Backoff,
    CircuitOpen,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginStartReason {
    Initial,
    DesiredState,
    Restart,
    CircuitProbe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginLifecycleAction {
    Start {
        generation: u64,
        reason: PluginStartReason,
    },
    Stop {
        generation: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginLifecycleSnapshot {
    pub(crate) plugin_id: String,
    pub(crate) desired_state: PluginDesiredState,
    pub(crate) available: bool,
    pub(crate) state: PluginOperationalState,
    pub(crate) state_changed_at: DateTime<Utc>,
    pub(crate) generation: u64,
    pub(crate) restart_attempts: u64,
    pub(crate) consecutive_failures: u32,
    pub(crate) last_started_at: Option<DateTime<Utc>>,
    pub(crate) last_stopped_at: Option<DateTime<Utc>>,
    pub(crate) last_healthy_at: Option<DateTime<Utc>>,
    pub(crate) last_error: Option<String>,
    pub(crate) last_error_at: Option<DateTime<Utc>>,
    pub(crate) next_retry_at: Option<DateTime<Utc>>,
}

/// Pure desired/actual state machine for one supervised plugin.
#[derive(Debug, Clone)]
pub(crate) struct PluginLifecycleMachine {
    plugin_id: String,
    desired_state: PluginDesiredState,
    available: bool,
    state: PluginOperationalState,
    state_changed_at: DateTime<Utc>,
    generation: u64,
    restart_attempts: u64,
    consecutive_failures: u32,
    last_started_at: Option<DateTime<Utc>>,
    last_stopped_at: Option<DateTime<Utc>>,
    last_healthy_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    last_error_at: Option<DateTime<Utc>>,
    next_retry_at: Option<DateTime<Utc>>,
}

impl PluginLifecycleMachine {
    pub(crate) fn new(
        plugin_id: impl Into<String>,
        available: bool,
        desired_state: PluginDesiredState,
        now: DateTime<Utc>,
    ) -> Self {
        let state = settled_state(available, desired_state);
        Self {
            plugin_id: plugin_id.into(),
            desired_state,
            available,
            state,
            state_changed_at: now,
            generation: 0,
            restart_attempts: 0,
            consecutive_failures: 0,
            last_started_at: None,
            last_stopped_at: None,
            last_healthy_at: None,
            last_error: None,
            last_error_at: None,
            next_retry_at: None,
        }
    }

    pub(crate) fn set_desired_state(&mut self, desired_state: PluginDesiredState) {
        self.desired_state = desired_state;
        if !desired_state.is_enabled() {
            self.next_retry_at = None;
        }
    }

    pub(crate) fn set_available(&mut self, available: bool) {
        self.available = available;
        if !available {
            self.next_retry_at = None;
        }
    }

    /// Clears failure recovery after an operator-controlled input changes.
    pub(crate) fn reset_recovery_after_input_change(&mut self, now: DateTime<Utc>) -> bool {
        if !matches!(
            self.state,
            PluginOperationalState::Backoff
                | PluginOperationalState::CircuitOpen
                | PluginOperationalState::Failed
        ) {
            return false;
        }
        self.consecutive_failures = 0;
        self.next_retry_at = None;
        self.transition_to(settled_state(self.available, self.desired_state), now);
        true
    }

    /// Advances toward desired state and reserves start/stop work before async I/O begins.
    pub(crate) fn reconcile(
        &mut self,
        now: DateTime<Utc>,
        policy: &PluginRestartPolicy,
    ) -> Option<PluginLifecycleAction> {
        self.reset_failure_streak_if_stable(now, policy);

        if !self.available || !self.desired_state.is_enabled() {
            return match self.state {
                PluginOperationalState::Starting | PluginOperationalState::Running => {
                    self.transition_to(PluginOperationalState::Stopping, now);
                    self.next_retry_at = None;
                    Some(PluginLifecycleAction::Stop {
                        generation: self.generation,
                    })
                }
                PluginOperationalState::Degraded => {
                    self.transition_to(PluginOperationalState::Stopping, now);
                    self.next_retry_at = None;
                    Some(PluginLifecycleAction::Stop {
                        generation: self.generation,
                    })
                }
                PluginOperationalState::Stopping => None,
                _ => {
                    self.transition_to(settled_state(self.available, self.desired_state), now);
                    self.next_retry_at = None;
                    self.consecutive_failures = 0;
                    None
                }
            };
        }

        let reason = match self.state {
            PluginOperationalState::Unavailable
            | PluginOperationalState::Disabled
            | PluginOperationalState::Stopped => {
                if self.generation == 0 {
                    Some(PluginStartReason::Initial)
                } else {
                    Some(PluginStartReason::DesiredState)
                }
            }
            PluginOperationalState::Backoff if self.retry_is_due(now) => {
                Some(PluginStartReason::Restart)
            }
            PluginOperationalState::CircuitOpen if self.retry_is_due(now) => {
                Some(PluginStartReason::CircuitProbe)
            }
            _ => None,
        }?;

        self.generation = self.generation.saturating_add(1);
        if matches!(
            reason,
            PluginStartReason::Restart | PluginStartReason::CircuitProbe
        ) {
            self.restart_attempts = self.restart_attempts.saturating_add(1);
        }
        self.next_retry_at = None;
        self.transition_to(PluginOperationalState::Starting, now);
        Some(PluginLifecycleAction::Start {
            generation: self.generation,
            reason,
        })
    }

    pub(crate) fn mark_running(
        &mut self,
        generation: u64,
        now: DateTime<Utc>,
    ) -> Result<(), PluginLifecycleTransitionError> {
        self.require_generation(generation, "mark_running")?;
        self.require_state(PluginOperationalState::Starting, "mark_running")?;
        self.transition_to(PluginOperationalState::Running, now);
        self.last_started_at = Some(now);
        self.last_healthy_at = Some(now);
        self.next_retry_at = None;
        Ok(())
    }

    /// Completes an intentional stop. Unexpected exits must be recorded with `mark_failed`.
    pub(crate) fn mark_stopped(
        &mut self,
        generation: u64,
        now: DateTime<Utc>,
    ) -> Result<(), PluginLifecycleTransitionError> {
        self.require_generation(generation, "mark_stopped")?;
        self.require_state(PluginOperationalState::Stopping, "mark_stopped")?;
        self.transition_to(settled_state(self.available, self.desired_state), now);
        self.last_stopped_at = Some(now);
        self.next_retry_at = None;
        self.consecutive_failures = 0;
        Ok(())
    }

    pub(crate) fn mark_failed(
        &mut self,
        generation: u64,
        error: impl Into<String>,
        now: DateTime<Utc>,
        policy: &PluginRestartPolicy,
    ) -> Result<(), PluginLifecycleTransitionError> {
        self.require_generation(generation, "mark_failed")?;
        if !matches!(
            self.state,
            PluginOperationalState::Starting
                | PluginOperationalState::Running
                | PluginOperationalState::Degraded
                | PluginOperationalState::Stopping
        ) {
            return Err(self.invalid_transition("mark_failed", None));
        }

        self.reset_failure_streak_if_stable(now, policy);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_stopped_at = Some(now);
        self.last_error = Some(clamp_error(error.into()));
        self.last_error_at = Some(now);

        if !self.available || !self.desired_state.is_enabled() {
            self.next_retry_at = None;
            self.consecutive_failures = 0;
            self.transition_to(settled_state(self.available, self.desired_state), now);
            return Ok(());
        }

        match policy.decision(self.consecutive_failures) {
            PluginRestartDecision::Backoff { delay } => {
                self.next_retry_at = Some(deadline_after(now, delay));
                self.transition_to(PluginOperationalState::Backoff, now);
            }
            PluginRestartDecision::OpenCircuit { cooldown } => {
                self.next_retry_at = Some(deadline_after(now, cooldown));
                self.transition_to(PluginOperationalState::CircuitOpen, now);
            }
        }
        Ok(())
    }

    /// Records a live but unhealthy process without consuming a restart attempt.
    #[allow(dead_code)]
    pub(crate) fn mark_degraded(
        &mut self,
        generation: u64,
        error: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<(), PluginLifecycleTransitionError> {
        self.require_generation(generation, "mark_degraded")?;
        if !matches!(
            self.state,
            PluginOperationalState::Running | PluginOperationalState::Degraded
        ) {
            return Err(self.invalid_transition("mark_degraded", None));
        }
        self.last_error = Some(clamp_error(error.into()));
        self.last_error_at = Some(now);
        self.transition_to(PluginOperationalState::Degraded, now);
        Ok(())
    }

    /// Records a non-retryable start/runtime error until configuration or availability changes.
    pub(crate) fn mark_terminal_failure(
        &mut self,
        generation: u64,
        error: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<(), PluginLifecycleTransitionError> {
        self.require_generation(generation, "mark_terminal_failure")?;
        if !matches!(
            self.state,
            PluginOperationalState::Starting
                | PluginOperationalState::Running
                | PluginOperationalState::Degraded
                | PluginOperationalState::Stopping
        ) {
            return Err(self.invalid_transition("mark_terminal_failure", None));
        }
        self.last_stopped_at = Some(now);
        self.last_error = Some(clamp_error(error.into()));
        self.last_error_at = Some(now);
        self.next_retry_at = None;
        self.transition_to(PluginOperationalState::Failed, now);
        Ok(())
    }

    /// Records liveness and clears the failure streak only after a stable run.
    pub(crate) fn mark_healthy(
        &mut self,
        generation: u64,
        now: DateTime<Utc>,
        policy: &PluginRestartPolicy,
    ) -> Result<bool, PluginLifecycleTransitionError> {
        self.require_generation(generation, "mark_healthy")?;
        if !matches!(
            self.state,
            PluginOperationalState::Running | PluginOperationalState::Degraded
        ) {
            return Err(self.invalid_transition("mark_healthy", None));
        }
        self.transition_to(PluginOperationalState::Running, now);
        self.last_healthy_at = Some(now);
        Ok(self.reset_failure_streak_if_stable(now, policy))
    }

    pub(crate) fn snapshot(&self) -> PluginLifecycleSnapshot {
        PluginLifecycleSnapshot {
            plugin_id: self.plugin_id.clone(),
            desired_state: self.desired_state,
            available: self.available,
            state: self.state,
            state_changed_at: self.state_changed_at,
            generation: self.generation,
            restart_attempts: self.restart_attempts,
            consecutive_failures: self.consecutive_failures,
            last_started_at: self.last_started_at,
            last_stopped_at: self.last_stopped_at,
            last_healthy_at: self.last_healthy_at,
            last_error: self.last_error.clone(),
            last_error_at: self.last_error_at,
            next_retry_at: self.next_retry_at,
        }
    }

    fn reset_failure_streak_if_stable(
        &mut self,
        now: DateTime<Utc>,
        policy: &PluginRestartPolicy,
    ) -> bool {
        if !matches!(
            self.state,
            PluginOperationalState::Running | PluginOperationalState::Degraded
        ) || self.consecutive_failures == 0
        {
            return false;
        }
        let Some(started_at) = self.last_started_at else {
            return false;
        };
        let Ok(running_for) = now.signed_duration_since(started_at).to_std() else {
            return false;
        };
        if !policy.has_stabilized(running_for) {
            return false;
        }

        self.consecutive_failures = 0;
        true
    }

    fn retry_is_due(&self, now: DateTime<Utc>) -> bool {
        self.next_retry_at
            .is_some_and(|next_retry_at| now >= next_retry_at)
    }

    fn transition_to(&mut self, state: PluginOperationalState, now: DateTime<Utc>) {
        if self.state != state {
            self.state = state;
            self.state_changed_at = now;
        }
    }

    fn require_state(
        &self,
        expected: PluginOperationalState,
        operation: &'static str,
    ) -> Result<(), PluginLifecycleTransitionError> {
        if self.state == expected {
            Ok(())
        } else {
            Err(self.invalid_transition(operation, None))
        }
    }

    fn require_generation(
        &self,
        generation: u64,
        operation: &'static str,
    ) -> Result<(), PluginLifecycleTransitionError> {
        if generation == self.generation {
            Ok(())
        } else {
            Err(self.invalid_transition(operation, Some(generation)))
        }
    }

    fn invalid_transition(
        &self,
        operation: &'static str,
        received_generation: Option<u64>,
    ) -> PluginLifecycleTransitionError {
        PluginLifecycleTransitionError {
            plugin_id: self.plugin_id.clone(),
            state: self.state,
            operation,
            expected_generation: self.generation,
            received_generation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginLifecycleTransitionError {
    pub(crate) plugin_id: String,
    pub(crate) state: PluginOperationalState,
    pub(crate) operation: &'static str,
    pub(crate) expected_generation: u64,
    pub(crate) received_generation: Option<u64>,
}

impl fmt::Display for PluginLifecycleTransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "plugin '{}' cannot {} while in {:?} (expected generation {}",
            self.plugin_id, self.operation, self.state, self.expected_generation
        )?;
        if let Some(received_generation) = self.received_generation {
            write!(formatter, ", received {received_generation}")?;
        }
        formatter.write_str(")")
    }
}

impl Error for PluginLifecycleTransitionError {}

fn settled_state(available: bool, desired_state: PluginDesiredState) -> PluginOperationalState {
    if !available {
        PluginOperationalState::Unavailable
    } else if desired_state.is_enabled() {
        PluginOperationalState::Stopped
    } else {
        PluginOperationalState::Disabled
    }
}

fn deadline_after(now: DateTime<Utc>, delay: Duration) -> DateTime<Utc> {
    let delta = TimeDelta::from_std(delay).unwrap_or(TimeDelta::MAX);
    now.checked_add_signed(delta)
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

fn clamp_error(error: String) -> String {
    let error = error.trim();
    if error.chars().count() <= MAX_RECORDED_ERROR_CHARS {
        return error.to_string();
    }
    error.chars().take(MAX_RECORDED_ERROR_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0).single().unwrap()
    }

    fn policy() -> PluginRestartPolicy {
        PluginRestartPolicy::new(
            Duration::from_secs(2),
            Duration::from_secs(10),
            3,
            Duration::from_secs(90),
            Duration::from_secs(30),
        )
        .unwrap()
    }

    #[test]
    fn reconcile_reserves_start_and_stop_transitions() {
        let mut lifecycle = PluginLifecycleMachine::new(
            "dev.ponderer.example",
            true,
            PluginDesiredState::Enabled,
            at(0),
        );

        assert_eq!(
            lifecycle.reconcile(at(1), &policy()),
            Some(PluginLifecycleAction::Start {
                generation: 1,
                reason: PluginStartReason::Initial,
            })
        );
        assert_eq!(lifecycle.snapshot().state, PluginOperationalState::Starting);
        lifecycle.mark_running(1, at(2)).unwrap();

        lifecycle.set_desired_state(PluginDesiredState::Disabled);
        assert_eq!(
            lifecycle.reconcile(at(3), &policy()),
            Some(PluginLifecycleAction::Stop { generation: 1 })
        );
        lifecycle.mark_stopped(1, at(4)).unwrap();
        let snapshot = lifecycle.snapshot();
        assert_eq!(snapshot.state, PluginOperationalState::Disabled);
        assert_eq!(snapshot.last_stopped_at, Some(at(4)));
    }

    #[test]
    fn failures_back_off_then_open_and_probe_the_circuit() {
        let policy = policy();
        let mut lifecycle = PluginLifecycleMachine::new(
            "dev.ponderer.example",
            true,
            PluginDesiredState::Enabled,
            at(0),
        );
        lifecycle.reconcile(at(0), &policy);
        lifecycle.mark_failed(1, "first", at(1), &policy).unwrap();
        assert_eq!(lifecycle.snapshot().next_retry_at, Some(at(3)));
        assert_eq!(lifecycle.reconcile(at(2), &policy), None);
        assert_eq!(
            lifecycle.reconcile(at(3), &policy),
            Some(PluginLifecycleAction::Start {
                generation: 2,
                reason: PluginStartReason::Restart,
            })
        );

        lifecycle.mark_failed(2, "second", at(4), &policy).unwrap();
        assert_eq!(lifecycle.snapshot().next_retry_at, Some(at(8)));
        lifecycle.reconcile(at(8), &policy);
        lifecycle.mark_failed(3, "third", at(9), &policy).unwrap();
        let open = lifecycle.snapshot();
        assert_eq!(open.state, PluginOperationalState::CircuitOpen);
        assert_eq!(open.next_retry_at, Some(at(99)));
        assert_eq!(lifecycle.reconcile(at(98), &policy), None);
        assert_eq!(
            lifecycle.reconcile(at(99), &policy),
            Some(PluginLifecycleAction::Start {
                generation: 4,
                reason: PluginStartReason::CircuitProbe,
            })
        );
        assert_eq!(lifecycle.snapshot().restart_attempts, 3);

        lifecycle.mark_running(4, at(100)).unwrap();
        assert!(!lifecycle.mark_healthy(4, at(129), &policy).unwrap());
        assert!(lifecycle.mark_healthy(4, at(130), &policy).unwrap());
        assert_eq!(lifecycle.snapshot().consecutive_failures, 0);
    }

    #[test]
    fn disabling_during_backoff_cancels_retry_and_resets_the_streak() {
        let policy = policy();
        let mut lifecycle = PluginLifecycleMachine::new(
            "dev.ponderer.example",
            true,
            PluginDesiredState::Enabled,
            at(0),
        );
        lifecycle.reconcile(at(0), &policy);
        lifecycle.mark_failed(1, "boom", at(1), &policy).unwrap();

        lifecycle.set_desired_state(PluginDesiredState::Disabled);
        assert_eq!(lifecycle.reconcile(at(2), &policy), None);
        let disabled = lifecycle.snapshot();
        assert_eq!(disabled.state, PluginOperationalState::Disabled);
        assert_eq!(disabled.next_retry_at, None);
        assert_eq!(disabled.consecutive_failures, 0);

        lifecycle.set_desired_state(PluginDesiredState::Enabled);
        assert_eq!(
            lifecycle.reconcile(at(3), &policy),
            Some(PluginLifecycleAction::Start {
                generation: 2,
                reason: PluginStartReason::DesiredState,
            })
        );
    }

    #[test]
    fn disappearing_package_is_stopped_and_can_be_rediscovered() {
        let policy = policy();
        let mut lifecycle = PluginLifecycleMachine::new(
            "dev.ponderer.example",
            true,
            PluginDesiredState::Enabled,
            at(0),
        );
        lifecycle.reconcile(at(0), &policy);
        lifecycle.mark_running(1, at(1)).unwrap();

        lifecycle.set_available(false);
        assert_eq!(
            lifecycle.reconcile(at(2), &policy),
            Some(PluginLifecycleAction::Stop { generation: 1 })
        );
        lifecycle.mark_stopped(1, at(3)).unwrap();
        assert_eq!(
            lifecycle.snapshot().state,
            PluginOperationalState::Unavailable
        );

        lifecycle.set_available(true);
        assert_eq!(
            lifecycle.reconcile(at(4), &policy),
            Some(PluginLifecycleAction::Start {
                generation: 2,
                reason: PluginStartReason::DesiredState,
            })
        );
    }

    #[test]
    fn stable_runtime_resets_prior_failures_before_a_later_exit() {
        let policy = policy();
        let mut lifecycle = PluginLifecycleMachine::new(
            "dev.ponderer.example",
            true,
            PluginDesiredState::Enabled,
            at(0),
        );
        lifecycle.reconcile(at(0), &policy);
        lifecycle.mark_failed(1, "first", at(1), &policy).unwrap();
        lifecycle.reconcile(at(3), &policy);
        lifecycle.mark_running(2, at(4)).unwrap();
        lifecycle.mark_failed(2, "later", at(34), &policy).unwrap();

        let snapshot = lifecycle.snapshot();
        assert_eq!(snapshot.consecutive_failures, 1);
        assert_eq!(snapshot.next_retry_at, Some(at(36)));
    }

    #[test]
    fn impossible_completion_is_rejected_without_mutation() {
        let mut lifecycle = PluginLifecycleMachine::new(
            "dev.ponderer.example",
            true,
            PluginDesiredState::Disabled,
            at(0),
        );
        let error = lifecycle.mark_running(0, at(1)).unwrap_err();

        assert_eq!(error.state, PluginOperationalState::Disabled);
        assert_eq!(error.operation, "mark_running");
        assert_eq!(lifecycle.snapshot().state, PluginOperationalState::Disabled);
    }

    #[test]
    fn recorded_errors_are_trimmed_and_bounded() {
        let policy = policy();
        let mut lifecycle = PluginLifecycleMachine::new(
            "dev.ponderer.example",
            true,
            PluginDesiredState::Enabled,
            at(0),
        );
        lifecycle.reconcile(at(0), &policy);
        lifecycle
            .mark_failed(1, format!("  {}  ", "x".repeat(2_100)), at(1), &policy)
            .unwrap();

        assert_eq!(
            lifecycle
                .snapshot()
                .last_error
                .expect("failure should be retained")
                .chars()
                .count(),
            MAX_RECORDED_ERROR_CHARS
        );
    }

    #[test]
    fn stale_generation_completion_is_rejected() {
        let policy = policy();
        let mut lifecycle = PluginLifecycleMachine::new(
            "dev.ponderer.example",
            true,
            PluginDesiredState::Enabled,
            at(0),
        );
        lifecycle.reconcile(at(0), &policy);
        lifecycle.mark_failed(1, "first", at(1), &policy).unwrap();
        lifecycle.reconcile(at(3), &policy);

        let error = lifecycle.mark_running(1, at(4)).unwrap_err();
        assert_eq!(error.expected_generation, 2);
        assert_eq!(error.received_generation, Some(1));
        assert_eq!(lifecycle.snapshot().state, PluginOperationalState::Starting);
    }

    #[test]
    fn degraded_process_can_recover_without_a_restart() {
        let policy = policy();
        let mut lifecycle = PluginLifecycleMachine::new(
            "dev.ponderer.example",
            true,
            PluginDesiredState::Enabled,
            at(0),
        );
        lifecycle.reconcile(at(0), &policy);
        lifecycle.mark_running(1, at(1)).unwrap();
        lifecycle
            .mark_degraded(1, "health check timed out", at(2))
            .unwrap();
        assert_eq!(lifecycle.snapshot().state, PluginOperationalState::Degraded);

        lifecycle.mark_healthy(1, at(3), &policy).unwrap();
        let recovered = lifecycle.snapshot();
        assert_eq!(recovered.state, PluginOperationalState::Running);
        assert_eq!(recovered.last_healthy_at, Some(at(3)));
    }

    #[test]
    fn terminal_failure_waits_for_operator_reconciliation() {
        let policy = policy();
        let mut lifecycle = PluginLifecycleMachine::new(
            "dev.ponderer.example",
            true,
            PluginDesiredState::Enabled,
            at(0),
        );
        lifecycle.reconcile(at(0), &policy);
        lifecycle
            .mark_terminal_failure(1, "invalid launch manifest", at(1))
            .unwrap();

        assert_eq!(lifecycle.snapshot().state, PluginOperationalState::Failed);
        assert_eq!(lifecycle.reconcile(at(100), &policy), None);
        lifecycle.set_desired_state(PluginDesiredState::Disabled);
        assert_eq!(lifecycle.reconcile(at(101), &policy), None);
        lifecycle.set_desired_state(PluginDesiredState::Enabled);
        assert_eq!(
            lifecycle.reconcile(at(102), &policy),
            Some(PluginLifecycleAction::Start {
                generation: 2,
                reason: PluginStartReason::DesiredState,
            })
        );
    }

    #[test]
    fn changed_operator_input_can_reset_terminal_failure() {
        let policy = policy();
        let mut lifecycle = PluginLifecycleMachine::new(
            "dev.ponderer.example",
            true,
            PluginDesiredState::Enabled,
            at(0),
        );
        lifecycle.reconcile(at(0), &policy);
        lifecycle
            .mark_terminal_failure(1, "invalid settings", at(1))
            .unwrap();

        assert!(lifecycle.reset_recovery_after_input_change(at(2)));
        assert_eq!(lifecycle.snapshot().state, PluginOperationalState::Stopped);
        assert_eq!(
            lifecycle.reconcile(at(2), &policy),
            Some(PluginLifecycleAction::Start {
                generation: 2,
                reason: PluginStartReason::DesiredState,
            })
        );
        assert!(!lifecycle.reset_recovery_after_input_change(at(3)));
    }

    #[test]
    fn changed_operator_input_closes_backoff_and_circuit() {
        let policy = policy();
        let mut lifecycle = PluginLifecycleMachine::new(
            "dev.ponderer.example",
            true,
            PluginDesiredState::Enabled,
            at(0),
        );
        lifecycle.reconcile(at(0), &policy);
        lifecycle.mark_failed(1, "first", at(1), &policy).unwrap();
        assert!(lifecycle.reset_recovery_after_input_change(at(2)));
        assert_eq!(lifecycle.snapshot().state, PluginOperationalState::Stopped);
        assert_eq!(lifecycle.snapshot().next_retry_at, None);

        lifecycle.reconcile(at(2), &policy);
        lifecycle.mark_failed(2, "one", at(3), &policy).unwrap();
        lifecycle.reconcile(at(5), &policy);
        lifecycle.mark_failed(3, "two", at(6), &policy).unwrap();
        lifecycle.reconcile(at(10), &policy);
        lifecycle.mark_failed(4, "three", at(11), &policy).unwrap();
        assert_eq!(
            lifecycle.snapshot().state,
            PluginOperationalState::CircuitOpen
        );
        assert!(lifecycle.reset_recovery_after_input_change(at(12)));
        assert_eq!(lifecycle.snapshot().state, PluginOperationalState::Stopped);
        assert_eq!(lifecycle.snapshot().consecutive_failures, 0);
    }
}
