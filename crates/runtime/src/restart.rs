use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestartPolicy {
    pub maximum_attempts: u32,
    pub initial_delay: Duration,
    pub maximum_delay: Duration,
    pub maximum_consecutive_health_failures: u32,
    pub healthy_reset_after: Duration,
    pub startup_timeout: Duration,
}

impl RestartPolicy {
    #[must_use]
    pub fn delay_for_attempt(self, attempt: u32) -> Option<Duration> {
        if attempt == 0 || attempt > self.maximum_attempts {
            return None;
        }
        let multiplier = 1u32
            .checked_shl(attempt.saturating_sub(1))
            .unwrap_or(u32::MAX);
        Some(
            self.initial_delay
                .saturating_mul(multiplier)
                .min(self.maximum_delay),
        )
    }
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            maximum_attempts: 3,
            initial_delay: Duration::from_millis(250),
            maximum_delay: Duration::from_secs(2),
            maximum_consecutive_health_failures: 3,
            healthy_reset_after: Duration::from_secs(60),
            startup_timeout: Duration::from_secs(180),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_delays_are_bounded() {
        let policy = RestartPolicy::default();
        assert_eq!(
            policy.delay_for_attempt(1),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            policy.delay_for_attempt(2),
            Some(Duration::from_millis(500))
        );
        assert_eq!(policy.delay_for_attempt(3), Some(Duration::from_secs(1)));
        assert_eq!(policy.delay_for_attempt(4), None);
        assert_eq!(policy.maximum_consecutive_health_failures, 3);
        assert_eq!(policy.healthy_reset_after, Duration::from_secs(60));
        assert_eq!(policy.startup_timeout, Duration::from_secs(180));
    }
}
