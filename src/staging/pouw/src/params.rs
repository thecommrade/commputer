/// All tunable knobs of the verification game. Fractions are basis points (/10_000).
#[derive(Clone, Debug)]
pub struct GameParams {
    pub k: usize,                  // proactive committee size
    pub k_escalate: usize,         // escalation panel size (> k)
    pub sample_rate_bps: u32,      // P(a job is proactively verified)
    pub p_trap_bps: u32,           // P(a verification round is a trap)
    pub quorum_num: usize,         // quorum = ceil(quorum_num/quorum_den * committee)
    pub quorum_den: usize,
    pub worker_bps: u32,           // settlement split of a Confirmed budget
    pub verifier_bps: u32,
    pub burn_bps: u32,
    pub executor_bond: u64,        // posted by the executor (>= budget in practice)
    pub verifier_bond: u64,        // posted by each committee/panel verifier
    pub challenger_bond: u64,      // posted by a challenger of an unsampled result
    pub dispute_bounty_bps: u32,   // committee-Disputed: share of slashed Be to honest verifiers
    pub challenger_reward_bps: u32,// challenge-Disputed: share of slashed Be to the challenger
    pub escalation_reward_bps: u32,// escalation: share of slashed bond to the panel
    pub trap_jackpot_bps: u32,     // trap: share of slashed rubber-stamper bonds to honest verifiers
}

impl Default for GameParams {
    fn default() -> Self {
        Self {
            k: 3, k_escalate: 7,
            sample_rate_bps: 10_000,     // start verifying every job; the sim sweeps this down
            p_trap_bps: 1_000,           // 10%
            quorum_num: 2, quorum_den: 3,
            worker_bps: 8_500, verifier_bps: 1_000, burn_bps: 500,
            executor_bond: 100, verifier_bond: 20, challenger_bond: 50,
            dispute_bounty_bps: 2_000,
            challenger_reward_bps: 1_000,
            escalation_reward_bps: 1_000,
            trap_jackpot_bps: 5_000,
        }
    }
}

impl GameParams {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.worker_bps + self.verifier_bps + self.burn_bps != 10_000 {
            return Err("settlement split must sum to 10_000 bps");
        }
        if self.challenger_reward_bps + self.escalation_reward_bps > 10_000 {
            return Err("escalation reward shares exceed the slashed bond");
        }
        if self.k_escalate <= self.k { return Err("k_escalate must exceed k"); }
        if self.quorum_den == 0 || self.quorum_num > self.quorum_den {
            return Err("bad quorum fraction");
        }
        Ok(())
    }

    /// Minimum agreeing votes for a quorum over `committee_size` participants.
    pub fn quorum(&self, committee_size: usize) -> usize {
        // ceil(quorum_num/quorum_den * committee_size)
        (self.quorum_num * committee_size + self.quorum_den - 1) / self.quorum_den
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_satisfy_invariants() {
        let p = GameParams::default();
        // settlement split sums to 100%
        assert_eq!(p.worker_bps + p.verifier_bps + p.burn_bps, 10_000);
        // escalation reward shares cannot exceed the slashed bond
        assert!(p.challenger_reward_bps + p.escalation_reward_bps <= 10_000);
        // escalation panel is larger than the committee
        assert!(p.k_escalate > p.k);
        // quorum is a real super-majority
        assert!(p.quorum_num * 2 >= p.quorum_den && p.quorum_num <= p.quorum_den);
        // a bond at least covers the value at risk
        assert!(p.executor_bond >= 1);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn validate_rejects_bad_split() {
        let mut p = GameParams::default();
        p.burn_bps += 1; // now sums to 10_001
        assert!(p.validate().is_err());
    }
}
