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

    // --- Fuel-pricing knobs (fuel-economics spec §3). ---
    /// Token units per 1,000,000 fuel — converts the engine's deterministic fuel
    /// metering into money. Consensus-visible market knob.
    pub price_per_mfuel: u64,
    /// Strict profitability margin (> 10_000) applied to budget_min's constraints.
    pub profit_margin_bps: u32,
    /// Safety multiplier (≥ 10_000) applied to both bond formulas.
    pub bond_safety_bps: u32,
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
            price_per_mfuel: 1,
            profit_margin_bps: 12_000,
            bond_safety_bps: 15_000,
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
        if self.price_per_mfuel == 0 {
            return Err("price_per_mfuel must be >= 1");
        }
        if self.profit_margin_bps <= 10_000 {
            return Err("profit_margin_bps must be a strict margin (> 10_000)");
        }
        if self.bond_safety_bps < 10_000 {
            return Err("bond_safety_bps must be >= 10_000");
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

    #[test]
    fn pricing_defaults_and_validation() {
        let p = GameParams::default();
        assert_eq!(p.price_per_mfuel, 1);
        assert_eq!(p.profit_margin_bps, 12_000);
        assert_eq!(p.bond_safety_bps, 15_000);
        assert!(p.validate().is_ok());

        // margin must be a STRICT margin (> 10_000): exact break-even would make an
        // at-minimum-funded honest role EV-zero and fail the sweep's strict EV-positive bar.
        let mut p = GameParams::default();
        p.profit_margin_bps = 10_000;
        assert!(p.validate().is_err());

        let mut p = GameParams::default();
        p.bond_safety_bps = 9_999;
        assert!(p.validate().is_err());

        let mut p = GameParams::default();
        p.bond_safety_bps = 10_000; // the inclusive valid edge — pins >= vs > distinction
        assert!(p.validate().is_ok());

        let mut p = GameParams::default();
        p.price_per_mfuel = 0;
        assert!(p.validate().is_err());
    }
}
