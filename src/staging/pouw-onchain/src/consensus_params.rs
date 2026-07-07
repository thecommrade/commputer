//! Module 8 — the consensus-params bundle + refuse-to-bind (blueprint G5 / phase P3).
//!
//! Aggregates every genesis-anchored consensus parameter (the WASM engine limits + the game/pricing
//! knobs + the P1 fees + the P2a phase windows + the P2c capacity split + the DA chunking + the
//! per-job fuel-cap floor) into one bundle. Provides: a canonical `fingerprint` (chain-identity
//! binding / drift detection), `validate` (internal consistency + priceability), `refuse_to_bind`
//! (the G5 startup assert — a node whose COMPILED wasmi config diverges from genesis refuses to
//! start rather than silently forking), and the per-job bounded fuel-cap admission.
//!
//! Derives only `Clone, Debug` (the frozen `GameParams` isn't `Eq`); the fingerprint is the
//! consensus-equality surface. WIRE-IN (founder/P3 patch-spec): encode this bundle in `genesis.json`,
//! call `refuse_to_bind(&WasmLimits::default())` at node startup (refuse on `Err`), and route the
//! `SubmitJob` admission path through `min_budget_for(declared_fuel_cap)`.

use commputer_pouw::economics::{budget_min, EconViolation};
use commputer_pouw::params::GameParams;
use commputer_pouw::wasm::WasmLimits;
use commputer_da::params::ChunkingParams;
use crate::capacity::CapacityParams;
use crate::lifecycle::PhaseDeadlines;
use crate::settlement_resolution::ResolutionParams;
use sha2::{Digest, Sha256};

/// Genesis-anchored phase durations in BLOCKS (P2a's `PhaseDeadlines` are absolute heights; genesis
/// anchors the window lengths, from which per-job deadlines are derived via `deadlines_for`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhaseWindows {
    pub result_blocks: u64,
    pub commit_blocks: u64,
    pub reveal_blocks: u64,
    /// G-E (Phase 1.1): how long a `SubmitJobV2` pending job may sit unclaimed —
    /// `claim_by = submit_height + claim_blocks`; past it the pot refunds to the submitter
    /// (`expire_pending_job`). Anchored at SUBMIT height; the other three windows anchor the
    /// per-job `PhaseDeadlines` at CLAIM height.
    pub claim_blocks: u64,
}

impl Default for PhaseWindows {
    fn default() -> Self {
        Self { result_blocks: 10, commit_blocks: 10, reveal_blocks: 10, claim_blocks: 10 }
    }
}

/// Every consensus-identical parameter anchored in genesis. Derives only `Clone, Debug` (the frozen
/// `GameParams` isn't `Eq`); use [`Self::fingerprint`] for equality.
#[derive(Clone, Debug)]
pub struct ConsensusParams {
    pub wasm_limits: WasmLimits,
    pub game: GameParams,
    pub resolution: ResolutionParams,
    pub phase_windows: PhaseWindows,
    pub capacity: CapacityParams,
    pub chunking: ChunkingParams,
    /// Per-job declared-cap floor (max = `wasm_limits.fuel`).
    pub min_fuel_cap: u64,
}

impl Default for ConsensusParams {
    fn default() -> Self {
        Self {
            wasm_limits: WasmLimits::default(),
            game: GameParams::default(),
            resolution: ResolutionParams::default(),
            phase_windows: PhaseWindows::default(),
            capacity: CapacityParams::default(),
            chunking: ChunkingParams::default(),
            // founder-picked safety floor: 1 mega-fuel (the work_cost pricing granularity).
            min_fuel_cap: 1_000_000,
        }
    }
}

/// A genesis params consistency failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamError {
    Game(&'static str),
    CancelBpsTooHigh(u32),
    TimeoutCompBpsTooHigh(u32),
    FlagshipReserveTooHigh(u32),
    ReserveMaxTooHigh(u32),
    ReserveChurnCoeffTooHigh(u32),
    ReserveFloorAboveMax { floor: u32, max: u32 },
    ZeroTotalSlots,
    ZeroPhaseWindow(&'static str),
    FuelCapBand { min: u64, max: u64 },
    Unpriceable(EconViolation),
}

/// A node cannot safely bind to this genesis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindError {
    Invalid(ParamError),
    WasmFingerprintMismatch { expected: [u8; 32], got: [u8; 32] },
}

/// A per-job declared fuel cap was rejected at admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FuelCapError {
    BelowMin { declared: u64, min: u64 },
    AboveMax { declared: u64, max: u64 },
    Unpriceable(EconViolation),
}

impl ConsensusParams {
    /// Canonical, domain-separated hash over every consensus param. The WASM-engine part uses
    /// `wasm_limits.config_fingerprint()` (already covers ENGINE_VERSION/ABI/fuel); every other field
    /// is folded as little-endian bytes in a fixed order (usize cast to u64 for platform-independence).
    /// Perturbing any field changes the output.
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"commputer-consensus-params-v1");
        h.update(self.wasm_limits.config_fingerprint());
        let g = &self.game;
        h.update((g.k as u64).to_le_bytes());
        h.update((g.k_escalate as u64).to_le_bytes());
        h.update(g.sample_rate_bps.to_le_bytes());
        h.update(g.p_trap_bps.to_le_bytes());
        h.update((g.quorum_num as u64).to_le_bytes());
        h.update((g.quorum_den as u64).to_le_bytes());
        h.update(g.worker_bps.to_le_bytes());
        h.update(g.verifier_bps.to_le_bytes());
        h.update(g.burn_bps.to_le_bytes());
        h.update(g.executor_bond.to_le_bytes());
        h.update(g.verifier_bond.to_le_bytes());
        h.update(g.challenger_bond.to_le_bytes());
        h.update(g.dispute_bounty_bps.to_le_bytes());
        h.update(g.challenger_reward_bps.to_le_bytes());
        h.update(g.escalation_reward_bps.to_le_bytes());
        h.update(g.trap_jackpot_bps.to_le_bytes());
        h.update(g.price_per_mfuel.to_le_bytes());
        h.update(g.profit_margin_bps.to_le_bytes());
        h.update(g.bond_safety_bps.to_le_bytes());
        h.update(self.resolution.cancel_burn_bps.to_le_bytes());
        h.update(self.resolution.timeout_submitter_comp_bps.to_le_bytes());
        h.update(self.phase_windows.result_blocks.to_le_bytes());
        h.update(self.phase_windows.commit_blocks.to_le_bytes());
        h.update(self.phase_windows.reveal_blocks.to_le_bytes());
        h.update(self.phase_windows.claim_blocks.to_le_bytes());
        let c = &self.capacity;
        h.update(c.total_slots.to_le_bytes());
        h.update(c.flagship_reserve_bps.to_le_bytes());
        h.update(c.reserve_floor_bps.to_le_bytes());
        h.update(c.reserve_max_bps.to_le_bytes());
        h.update(c.reserve_churn_coeff_bps.to_le_bytes());
        h.update(self.chunking.chunk_size.to_le_bytes());
        h.update(self.chunking.params_version.to_le_bytes());
        h.update(self.min_fuel_cap.to_le_bytes());
        h.finalize().into()
    }

    /// Internal consistency of the bundle (catches a malformed genesis before binding).
    pub fn validate(&self) -> Result<(), ParamError> {
        self.game.validate().map_err(ParamError::Game)?;
        let r = &self.resolution;
        if r.cancel_burn_bps > 10_000 {
            return Err(ParamError::CancelBpsTooHigh(r.cancel_burn_bps));
        }
        if r.timeout_submitter_comp_bps > 10_000 {
            return Err(ParamError::TimeoutCompBpsTooHigh(r.timeout_submitter_comp_bps));
        }
        let c = &self.capacity;
        if c.flagship_reserve_bps > 10_000 {
            return Err(ParamError::FlagshipReserveTooHigh(c.flagship_reserve_bps));
        }
        if c.reserve_max_bps > 10_000 {
            return Err(ParamError::ReserveMaxTooHigh(c.reserve_max_bps));
        }
        if c.reserve_churn_coeff_bps > 10_000 {
            // >100% added reserve at full churn is nonsensical (genesis-sanity symmetry with the
            // other capacity bps; the dynamic-reserve clamp already keeps determinism regardless).
            return Err(ParamError::ReserveChurnCoeffTooHigh(c.reserve_churn_coeff_bps));
        }
        if c.reserve_floor_bps > c.reserve_max_bps {
            return Err(ParamError::ReserveFloorAboveMax { floor: c.reserve_floor_bps, max: c.reserve_max_bps });
        }
        if c.total_slots == 0 {
            return Err(ParamError::ZeroTotalSlots);
        }
        let w = &self.phase_windows;
        if w.result_blocks == 0 { return Err(ParamError::ZeroPhaseWindow("result")); }
        if w.commit_blocks == 0 { return Err(ParamError::ZeroPhaseWindow("commit")); }
        if w.reveal_blocks == 0 { return Err(ParamError::ZeroPhaseWindow("reveal")); }
        if w.claim_blocks == 0 { return Err(ParamError::ZeroPhaseWindow("claim")); }
        if self.min_fuel_cap == 0 || self.min_fuel_cap > self.wasm_limits.fuel {
            return Err(ParamError::FuelCapBand { min: self.min_fuel_cap, max: self.wasm_limits.fuel });
        }
        // Priceability: one call suffices — budget_min's degenerate-params guard depends only on
        // GameParams, not fuel_cap, so a single bound proves the regime can price any job.
        budget_min(self.min_fuel_cap, &self.game).map_err(ParamError::Unpriceable)?;
        Ok(())
    }

    /// The G5 startup assert: the bundle is internally valid AND the node's COMPILED wasmi config
    /// matches the genesis-declared one (else the node would produce divergent execution digests and
    /// must refuse to start). The other params are loaded from genesis as data, so only the
    /// compiled-engine part needs a runtime match.
    pub fn refuse_to_bind(&self, compiled: &WasmLimits) -> Result<(), BindError> {
        self.validate().map_err(BindError::Invalid)?;
        let expected = self.wasm_limits.config_fingerprint();
        let got = compiled.config_fingerprint();
        if got != expected {
            return Err(BindError::WasmFingerprintMismatch { expected, got });
        }
        Ok(())
    }

    /// Validate a per-job declared fuel cap against the genesis band `[min_fuel_cap, wasm_limits.fuel]`.
    pub fn admit_fuel_cap(&self, declared: u64) -> Result<u64, FuelCapError> {
        if declared < self.min_fuel_cap {
            return Err(FuelCapError::BelowMin { declared, min: self.min_fuel_cap });
        }
        if declared > self.wasm_limits.fuel {
            return Err(FuelCapError::AboveMax { declared, max: self.wasm_limits.fuel });
        }
        Ok(declared)
    }

    /// The minimum budget a job declaring `declared` fuel must post: validate the cap, then price it
    /// via the frozen `budget_min`. The admission path rejects an underfunded job against this.
    pub fn min_budget_for(&self, declared: u64) -> Result<u64, FuelCapError> {
        let cap = self.admit_fuel_cap(declared)?;
        budget_min(cap, &self.game).map_err(FuelCapError::Unpriceable)
    }

    /// Derive a job's P2a per-job deadlines from the genesis phase windows (`submit_height` is the
    /// height the job's result is due from). Saturating adds.
    pub fn deadlines_for(&self, submit_height: u64) -> PhaseDeadlines {
        let result_by = submit_height.saturating_add(self.phase_windows.result_blocks);
        let commit_by = result_by.saturating_add(self.phase_windows.commit_blocks);
        let reveal_by = commit_by.saturating_add(self.phase_windows.reveal_blocks);
        PhaseDeadlines { result_by, commit_by, reveal_by }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bundle_constructs() {
        let cp = ConsensusParams::default();
        assert_eq!(cp.min_fuel_cap, 1_000_000);
        assert_eq!(cp.wasm_limits.fuel, 100_000_000);
        assert_eq!(cp.phase_windows, PhaseWindows::default());
        assert_eq!(cp.phase_windows.claim_blocks, 10, "G-E default claim window");
    }

    #[test]
    fn fingerprint_deterministic_and_field_sensitive() {
        let base = ConsensusParams::default();
        assert_eq!(base.fingerprint(), ConsensusParams::default().fingerprint(), "deterministic");
        // perturbing any component must change the fingerprint
        let mut a = base.clone(); a.game.worker_bps += 1; a.game.burn_bps -= 1; // keep sum, change a field
        assert_ne!(a.fingerprint(), base.fingerprint(), "game field");
        let mut b = base.clone(); b.resolution.cancel_burn_bps += 1;
        assert_ne!(b.fingerprint(), base.fingerprint(), "resolution field");
        let mut c = base.clone(); c.phase_windows.commit_blocks += 1;
        assert_ne!(c.fingerprint(), base.fingerprint(), "phase window");
        let mut cb = base.clone(); cb.phase_windows.claim_blocks += 1;
        assert_ne!(cb.fingerprint(), base.fingerprint(), "claim window (G-E)");
        let mut d = base.clone(); d.capacity.total_slots += 1;
        assert_ne!(d.fingerprint(), base.fingerprint(), "capacity field");
        let mut e = base.clone(); e.chunking.chunk_size += 1;
        assert_ne!(e.fingerprint(), base.fingerprint(), "chunking field");
        let mut f = base.clone(); f.min_fuel_cap += 1;
        assert_ne!(f.fingerprint(), base.fingerprint(), "min_fuel_cap");
        let mut g = base.clone(); g.wasm_limits.fuel += 1;
        assert_ne!(g.fingerprint(), base.fingerprint(), "wasm fuel (via config_fingerprint)");
    }

    #[test]
    fn default_validates_and_binds() {
        let cp = ConsensusParams::default();
        assert!(cp.validate().is_ok());
        assert!(cp.refuse_to_bind(&WasmLimits::default()).is_ok());
    }

    #[test]
    fn validate_rejects_each_violation() {
        // bad game bps sum (frozen GameParams::validate fires)
        let mut a = ConsensusParams::default(); a.game.burn_bps += 1;
        assert!(matches!(a.validate(), Err(ParamError::Game(_))));
        // resolution bps over 10_000
        let mut b = ConsensusParams::default(); b.resolution.cancel_burn_bps = 10_001;
        assert_eq!(b.validate(), Err(ParamError::CancelBpsTooHigh(10_001)));
        // reserve floor > max
        let mut c = ConsensusParams::default();
        c.capacity.reserve_floor_bps = 2_000; c.capacity.reserve_max_bps = 1_500;
        assert!(matches!(c.validate(), Err(ParamError::ReserveFloorAboveMax { .. })));
        // reserve churn coefficient over 10_000 (>100% added reserve at full churn)
        let mut cc = ConsensusParams::default(); cc.capacity.reserve_churn_coeff_bps = 10_001;
        assert_eq!(cc.validate(), Err(ParamError::ReserveChurnCoeffTooHigh(10_001)));
        // zero phase window
        let mut d = ConsensusParams::default(); d.phase_windows.commit_blocks = 0;
        assert_eq!(d.validate(), Err(ParamError::ZeroPhaseWindow("commit")));
        // zero claim window (G-E)
        let mut dc = ConsensusParams::default(); dc.phase_windows.claim_blocks = 0;
        assert_eq!(dc.validate(), Err(ParamError::ZeroPhaseWindow("claim")));
        // min_fuel_cap above the wasm fuel cap
        let mut e = ConsensusParams::default(); e.min_fuel_cap = e.wasm_limits.fuel + 1;
        assert!(matches!(e.validate(), Err(ParamError::FuelCapBand { .. })));
        // unpriceable regime: worker_bps 0 (sum still 10_000 so GameParams::validate passes, but
        // budget_min's guard rejects worker_bps==0)
        let mut f = ConsensusParams::default();
        f.game.worker_bps = 0; f.game.verifier_bps = 9_500; f.game.burn_bps = 500;
        assert!(matches!(f.validate(), Err(ParamError::Unpriceable(_))));
    }

    #[test]
    fn refuse_to_bind_checks_wasm_fingerprint() {
        let cp = ConsensusParams::default();
        assert!(cp.refuse_to_bind(&WasmLimits::default()).is_ok());
        // a node compiled with a different fuel cap ⇒ different config_fingerprint ⇒ refuse
        let mut diff = WasmLimits::default(); diff.fuel += 1;
        assert!(matches!(cp.refuse_to_bind(&diff), Err(BindError::WasmFingerprintMismatch { .. })));
        // an invalid bundle is rejected even with a matching compiled WasmLimits (validate runs first)
        let mut bad = ConsensusParams::default(); bad.capacity.total_slots = 0;
        assert!(matches!(bad.refuse_to_bind(&WasmLimits::default()), Err(BindError::Invalid(_))));
    }

    #[test]
    fn admit_fuel_cap_bounds() {
        let cp = ConsensusParams::default(); // min 1_000_000, max wasm fuel 100_000_000
        assert_eq!(cp.admit_fuel_cap(cp.min_fuel_cap), Ok(cp.min_fuel_cap));
        assert_eq!(cp.admit_fuel_cap(cp.wasm_limits.fuel), Ok(cp.wasm_limits.fuel));
        assert!(matches!(cp.admit_fuel_cap(cp.min_fuel_cap - 1), Err(FuelCapError::BelowMin { .. })));
        assert!(matches!(cp.admit_fuel_cap(cp.wasm_limits.fuel + 1), Err(FuelCapError::AboveMax { .. })));
    }

    #[test]
    fn min_budget_for_ties_to_budget_min() {
        let cp = ConsensusParams::default();
        let declared = 50_000_000u64; // in band
        assert_eq!(cp.min_budget_for(declared), Ok(budget_min(declared, &cp.game).unwrap()));
        assert!(matches!(cp.min_budget_for(cp.min_fuel_cap - 1), Err(FuelCapError::BelowMin { .. })));
    }

    #[test]
    fn deadlines_for_derives_heights() {
        let cp = ConsensusParams::default(); // windows {10,10,10}
        let d = cp.deadlines_for(100);
        assert_eq!(d.result_by, 110);
        assert_eq!(d.commit_by, 120);
        assert_eq!(d.reveal_by, 130);
    }
}
