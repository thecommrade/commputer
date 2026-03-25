/// Lifecycle status of a validator node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidatorStatus {
    Idle,
    Active,
}

/// State machine tracking a validator's participation in the network.
///
/// Transitions: Idle → Active (on `register`) → Idle (on `deregister`).
/// While Active, `contribution_percent` tracks how much of its resources
/// the validator is currently contributing (0–100).
#[derive(Debug, Clone)]
pub struct ValidatorState {
    status: ValidatorStatus,
    contribution_percent: u8,
}

impl ValidatorState {
    /// Create a new validator, starting in the Idle state.
    pub fn new() -> Self {
        Self {
            status: ValidatorStatus::Idle,
            contribution_percent: 0,
        }
    }

    /// Returns the current lifecycle status.
    pub fn status(&self) -> ValidatorStatus {
        self.status
    }

    /// Returns the current contribution percentage (0–100).
    /// Only meaningful while Active.
    pub fn contribution_percent(&self) -> u8 {
        self.contribution_percent
    }

    /// Transition from Idle to Active and set the initial contribution percentage.
    /// No-op if already Active.
    pub fn register(&mut self, contribution_percent: u8) {
        self.status = ValidatorStatus::Active;
        self.contribution_percent = contribution_percent;
    }

    /// Update contribution percentage while Active.
    /// No-op if Idle.
    pub fn update_contribution(&mut self, contribution_percent: u8) {
        if self.status == ValidatorStatus::Active {
            self.contribution_percent = contribution_percent;
        }
    }

    /// Transition from Active back to Idle.
    /// Resets contribution percentage. No-op if already Idle.
    pub fn deregister(&mut self) {
        self.status = ValidatorStatus::Idle;
        self.contribution_percent = 0;
    }
}

impl Default for ValidatorState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_validator_starts_idle() {
        let v = ValidatorState::new();
        assert_eq!(v.status(), ValidatorStatus::Idle);
    }

    #[test]
    fn register_transitions_to_active() {
        let mut v = ValidatorState::new();
        v.register(50);
        assert_eq!(v.status(), ValidatorStatus::Active);
        assert_eq!(v.contribution_percent(), 50);
    }

    #[test]
    fn update_contribution() {
        let mut v = ValidatorState::new();
        v.register(50);
        v.update_contribution(80);
        assert_eq!(v.contribution_percent(), 80);
    }

    #[test]
    fn deregister_transitions_to_idle() {
        let mut v = ValidatorState::new();
        v.register(50);
        v.deregister();
        assert_eq!(v.status(), ValidatorStatus::Idle);
    }
}
