use std::error::Error;
use std::fmt;
use std::time::Duration;

const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(30);
const DEFAULT_CIRCUIT_FAILURE_THRESHOLD: u32 = 5;
const DEFAULT_CIRCUIT_OPEN_DURATION: Duration = Duration::from_secs(5 * 60);
const DEFAULT_STABLE_RUN_DURATION: Duration = Duration::from_secs(60);

/// Decides whether a failed plugin should retry normally or pause behind a circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginRestartDecision {
    Backoff { delay: Duration },
    OpenCircuit { cooldown: Duration },
}

/// Bounded retry and recovery policy shared by every supervised plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PluginRestartPolicy {
    initial_backoff: Duration,
    max_backoff: Duration,
    circuit_failure_threshold: u32,
    circuit_open_duration: Duration,
    stable_run_duration: Duration,
}

impl PluginRestartPolicy {
    pub(crate) fn new(
        initial_backoff: Duration,
        max_backoff: Duration,
        circuit_failure_threshold: u32,
        circuit_open_duration: Duration,
        stable_run_duration: Duration,
    ) -> Result<Self, PluginRestartPolicyError> {
        if initial_backoff.is_zero() {
            return Err(PluginRestartPolicyError::ZeroInitialBackoff);
        }
        if max_backoff < initial_backoff {
            return Err(PluginRestartPolicyError::MaxBackoffBeforeInitial);
        }
        if circuit_failure_threshold == 0 {
            return Err(PluginRestartPolicyError::ZeroCircuitFailureThreshold);
        }
        if circuit_open_duration.is_zero() {
            return Err(PluginRestartPolicyError::ZeroCircuitOpenDuration);
        }

        Ok(Self {
            initial_backoff,
            max_backoff,
            circuit_failure_threshold,
            circuit_open_duration,
            stable_run_duration,
        })
    }

    pub(crate) fn decision(&self, consecutive_failures: u32) -> PluginRestartDecision {
        if consecutive_failures >= self.circuit_failure_threshold {
            return PluginRestartDecision::OpenCircuit {
                cooldown: self.circuit_open_duration,
            };
        }

        PluginRestartDecision::Backoff {
            delay: self.backoff_delay(consecutive_failures),
        }
    }

    pub(crate) fn has_stabilized(&self, running_for: Duration) -> bool {
        running_for >= self.stable_run_duration
    }

    fn backoff_delay(&self, consecutive_failures: u32) -> Duration {
        let exponent = consecutive_failures.saturating_sub(1).min(31);
        let multiplier = 1_u32 << exponent;
        self.initial_backoff
            .saturating_mul(multiplier)
            .min(self.max_backoff)
    }
}

impl Default for PluginRestartPolicy {
    fn default() -> Self {
        Self::new(
            DEFAULT_INITIAL_BACKOFF,
            DEFAULT_MAX_BACKOFF,
            DEFAULT_CIRCUIT_FAILURE_THRESHOLD,
            DEFAULT_CIRCUIT_OPEN_DURATION,
            DEFAULT_STABLE_RUN_DURATION,
        )
        .expect("default plugin restart policy is valid")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginRestartPolicyError {
    ZeroInitialBackoff,
    MaxBackoffBeforeInitial,
    ZeroCircuitFailureThreshold,
    ZeroCircuitOpenDuration,
}

impl fmt::Display for PluginRestartPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ZeroInitialBackoff => "initial plugin restart backoff must be non-zero",
            Self::MaxBackoffBeforeInitial => {
                "maximum plugin restart backoff must not be shorter than the initial backoff"
            }
            Self::ZeroCircuitFailureThreshold => {
                "plugin circuit failure threshold must be greater than zero"
            }
            Self::ZeroCircuitOpenDuration => "plugin circuit-open duration must be non-zero",
        };
        formatter.write_str(message)
    }
}

impl Error for PluginRestartPolicyError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PluginRestartPolicy {
        PluginRestartPolicy::new(
            Duration::from_secs(2),
            Duration::from_secs(10),
            5,
            Duration::from_secs(90),
            Duration::from_secs(30),
        )
        .unwrap()
    }

    #[test]
    fn exponential_backoff_is_bounded() {
        let policy = policy();
        let delays = (1..5)
            .map(|failure| match policy.decision(failure) {
                PluginRestartDecision::Backoff { delay } => delay,
                PluginRestartDecision::OpenCircuit { .. } => panic!("circuit opened too early"),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            delays,
            vec![
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(10),
            ]
        );
    }

    #[test]
    fn threshold_failure_opens_the_circuit() {
        assert_eq!(
            policy().decision(5),
            PluginRestartDecision::OpenCircuit {
                cooldown: Duration::from_secs(90)
            }
        );
    }

    #[test]
    fn stable_run_boundary_is_inclusive() {
        let policy = policy();
        assert!(!policy.has_stabilized(Duration::from_secs(29)));
        assert!(policy.has_stabilized(Duration::from_secs(30)));
    }

    #[test]
    fn invalid_policies_are_rejected() {
        assert_eq!(
            PluginRestartPolicy::new(
                Duration::ZERO,
                Duration::from_secs(1),
                1,
                Duration::from_secs(1),
                Duration::ZERO,
            ),
            Err(PluginRestartPolicyError::ZeroInitialBackoff)
        );
        assert_eq!(
            PluginRestartPolicy::new(
                Duration::from_secs(2),
                Duration::from_secs(1),
                1,
                Duration::from_secs(1),
                Duration::ZERO,
            ),
            Err(PluginRestartPolicyError::MaxBackoffBeforeInitial)
        );
        assert_eq!(
            PluginRestartPolicy::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                0,
                Duration::from_secs(1),
                Duration::ZERO,
            ),
            Err(PluginRestartPolicyError::ZeroCircuitFailureThreshold)
        );
        assert_eq!(
            PluginRestartPolicy::new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                1,
                Duration::ZERO,
                Duration::ZERO,
            ),
            Err(PluginRestartPolicyError::ZeroCircuitOpenDuration)
        );
    }

    #[test]
    fn policy_constructor_retains_effective_configuration() {
        let policy = policy();
        assert_eq!(policy.initial_backoff, Duration::from_secs(2));
        assert_eq!(policy.max_backoff, Duration::from_secs(10));
        assert_eq!(policy.circuit_failure_threshold, 5);
        assert_eq!(policy.circuit_open_duration, Duration::from_secs(90));
        assert_eq!(policy.stable_run_duration, Duration::from_secs(30));
    }
}
