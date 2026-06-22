//! Module 7 — the per-block compute-job admission accountant (whitepaper Core Principle #1, G6).
//!
//! Deterministic, dependency-free, consensus-pure: subtract the dynamic churn-based reserve, then
//! allocate the remaining per-block capacity with the 51/49 flagship-priority split (work-conserving,
//! dual floor). The staging PoUW game has no routing/priority/capacity concept (blueprint G6 gap);
//! this is the missing admission gate. INDEPENDENT of the verification lifecycle — an upstream gate
//! deciding which SubmitJobs enter a block's capacity; admitted jobs then flow into the JobLifecycle.
//!
//! WIRE-IN (founder note / P2 patch-spec): the node computes `churn_bps` from the validator-set delta
//! per epoch, calls `admit` at the SubmitJob admission path each block, and defers non-admitted jobs
//! to the mempool. `CapacityParams` is anchored in genesis. `is_flagship` = `l2_id == FLAGSHIP_L2_ID`
//! (core/src/l2.rs:48). The emergency 51%->data-protection redeploy (WHITEPAPER.md:336) and the
//! per-tier equal division among holders (line 37) are separate concerns, out of scope here.

/// Per-block capacity + reserve knobs (genesis params). Mirrors the PROTECTED core constants
/// FLAGSHIP_COMPUTE_SHARE/FLAGSHIP_CAPACITY_PERCENT = 51 and the §41 reserve formula.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapacityParams {
    /// Total compute-job slots admittable per block.
    pub total_slots: u32,
    /// Flagship reservation (5_100 = 51%).
    pub flagship_reserve_bps: u32,
    /// Minimum dynamic reserve (500 = 5%).
    pub reserve_floor_bps: u32,
    /// Maximum dynamic reserve (1_500 = 15%).
    pub reserve_max_bps: u32,
    /// Reserve added per full churn (1_000 = 10% of capacity at 100% churn).
    pub reserve_churn_coeff_bps: u32,
}

impl Default for CapacityParams {
    fn default() -> Self {
        Self {
            total_slots: 100,
            flagship_reserve_bps: 5_100,
            reserve_floor_bps: 500,
            reserve_max_bps: 1_500,
            reserve_churn_coeff_bps: 1_000,
        }
    }
}

/// A compute job awaiting admission this block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingJob {
    pub job_id: [u8; 32],
    /// `l2_id == FLAGSHIP_L2_ID` (resolved by the chain).
    pub is_flagship: bool,
    /// Deterministic ordering key (fee / submission order — the chain's choice).
    pub priority: u64,
}

/// The per-block admission decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    /// Admitted this block (flagship first, then shared).
    pub admitted: Vec<[u8; 32]>,
    /// Deferred to a later block.
    pub deferred: Vec<[u8; 32]>,
    pub flagship_admitted: u32,
    pub shared_admitted: u32,
    /// Slots available after the dynamic reserve.
    pub available: u32,
    /// The reserve applied (bps).
    pub reserve_bps: u32,
}

/// Dynamic reserve held back before the 51/49 split (Whitepaper §41): 5% + 10%·churn, clamped 5-15%.
/// `churn_bps` ∈ [0, 10_000] is the fraction of validators that joined/left last epoch.
pub fn dynamic_reserve_bps(p: &CapacityParams, churn_bps: u32) -> u32 {
    let raw = p.reserve_floor_bps as u64
        + (churn_bps as u64 * p.reserve_churn_coeff_bps as u64) / 10_000;
    (raw as u32).clamp(p.reserve_floor_bps, p.reserve_max_bps)
}

/// Per-block slots available after the dynamic reserve is held back (the reserve is subtracted
/// FIRST; `div_ceil` holds at least the reserve — the safe direction for a safety buffer).
pub fn available_slots(p: &CapacityParams, churn_bps: u32) -> u32 {
    let reserve_bps = dynamic_reserve_bps(p, churn_bps);
    let reserve_slots = ((p.total_slots as u64 * reserve_bps as u64).div_ceil(10_000)) as u32;
    p.total_slots.saturating_sub(reserve_slots)
}

/// Admit pending compute jobs into this block's capacity per the 51/49 work-conserving split.
/// Flagship is the HARD floor: flagship jobs are guaranteed `ceil(51% · available)` slots when they
/// have the demand (Core Principle #1, protocol-enforced). Non-flagship jobs get the remainder
/// (`available − flagship_floor`, i.e. ~49% — integer rounding biases the indivisible slot toward
/// the flagship floor, so the shared share can sit a hair under 49%). Slack from either class
/// underutilizing flows to the other (flagship-priority), so no slot idles while demand remains and
/// neither class takes the other's guaranteed slots.
/// Deterministic AND input-order-independent: a pure function of `(p, churn_bps, pending)` whose
/// result is fixed by the total-order key (priority desc, job_id asc) over unique job_ids.
pub fn admit(p: &CapacityParams, churn_bps: u32, pending: &[PendingJob]) -> Admission {
    let reserve_bps = dynamic_reserve_bps(p, churn_bps);
    let available = available_slots(p, churn_bps);
    let flag_reserve =
        (((available as u64 * p.flagship_reserve_bps as u64).div_ceil(10_000)) as u32).min(available);
    let shared_reserve = available - flag_reserve;

    // Split by class, each sorted by (priority desc, job_id asc) — a total order (job_ids unique).
    let mut flagship: Vec<&PendingJob> = pending.iter().filter(|j| j.is_flagship).collect();
    let mut shared: Vec<&PendingJob> = pending.iter().filter(|j| !j.is_flagship).collect();
    let by_priority = |a: &&PendingJob, b: &&PendingJob| {
        b.priority.cmp(&a.priority).then(a.job_id.cmp(&b.job_id))
    };
    flagship.sort_by(by_priority);
    shared.sort_by(by_priority);

    let f = flagship.len() as u32;
    let n = shared.len() as u32;
    // Floors.
    let mut flag_admit = f.min(flag_reserve);
    let mut shared_admit = n.min(shared_reserve);
    // Work-conserving slack fill (flagship-priority).
    let mut slack = available - flag_admit - shared_admit;
    let extra_flag = (f - flag_admit).min(slack);
    flag_admit += extra_flag;
    slack -= extra_flag;
    let extra_shared = (n - shared_admit).min(slack);
    shared_admit += extra_shared;

    let mut admitted = Vec::new();
    let mut deferred = Vec::new();
    for (i, j) in flagship.iter().enumerate() {
        if (i as u32) < flag_admit { admitted.push(j.job_id) } else { deferred.push(j.job_id) }
    }
    for (i, j) in shared.iter().enumerate() {
        if (i as u32) < shared_admit { admitted.push(j.job_id) } else { deferred.push(j.job_id) }
    }

    Admission {
        admitted,
        deferred,
        flagship_admitted: flag_admit,
        shared_admitted: shared_admit,
        available,
        reserve_bps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distinct job_id per `id` byte (unique ⇒ total ordering).
    fn job(id: u8, is_flagship: bool, priority: u64) -> PendingJob {
        PendingJob { job_id: [id; 32], is_flagship, priority }
    }

    #[test]
    fn defaults_match_principle_1() {
        let p = CapacityParams::default();
        assert_eq!(p.flagship_reserve_bps, 5_100); // 51%
        assert_eq!(p.reserve_floor_bps, 500);      // 5%
        assert_eq!(p.reserve_max_bps, 1_500);      // 15%
        assert_eq!(p.reserve_churn_coeff_bps, 1_000);
        // `job` helper is exercised by later tasks' tests.
        let _ = job(1, true, 1);
    }

    #[test]
    fn reserve_formula_matches_whitepaper() {
        let p = CapacityParams::default();
        assert_eq!(dynamic_reserve_bps(&p, 100), 510);    // 1% churn -> 5.1%
        assert_eq!(dynamic_reserve_bps(&p, 5_000), 1_000); // 50% -> 10%
        assert_eq!(dynamic_reserve_bps(&p, 10_000), 1_500); // 100% -> 15%
        assert_eq!(dynamic_reserve_bps(&p, 0), 500);       // floor 5%
        assert_eq!(dynamic_reserve_bps(&p, 20_000), 1_500); // >100% clamps to max
    }

    #[test]
    fn available_subtracts_reserve_ceil() {
        let p = CapacityParams::default(); // total 100
        assert_eq!(available_slots(&p, 0), 95);       // reserve 5% = 5 -> 95
        assert_eq!(available_slots(&p, 10_000), 85);  // reserve 15% = 15 -> 85
        assert_eq!(available_slots(&p, 100), 94);     // reserve 510bps -> ceil(5.1)=6 -> 94
        // degenerate: reserve rounds up to >= total -> 0 available
        let tiny = CapacityParams { total_slots: 1, ..CapacityParams::default() };
        assert_eq!(available_slots(&tiny, 0), 0); // ceil(1*0.05)=1 -> 0
    }

    #[test]
    fn both_oversubscribed_honors_51_49_floors() {
        let p = CapacityParams::default(); // total 100, churn 0 -> available 95
        // available 95: flag_reserve=ceil(95*0.51)=ceil(48.45)=49, shared_reserve=46
        let mut pending = Vec::new();
        for i in 0..60u8 { pending.push(job(i, true, 100)); }    // 60 flagship (> 49)
        for i in 60..120u8 { pending.push(job(i, false, 100)); } // 60 shared (> 46)
        let a = admit(&p, 0, &pending);
        assert_eq!(a.available, 95);
        assert_eq!(a.flagship_admitted, 49);
        assert_eq!(a.shared_admitted, 46);
        assert_eq!(a.admitted.len(), 95);
        assert_eq!(a.admitted.len() + a.deferred.len(), pending.len());
    }

    #[test]
    fn flagship_underutilizes_slack_to_shared() {
        let p = CapacityParams::default(); // available 95, flag_reserve 49, shared_reserve 46
        let mut pending = Vec::new();
        for i in 0..10u8 { pending.push(job(i, true, 50)); }     // only 10 flagship
        for i in 10..120u8 { pending.push(job(i, false, 50)); }  // 110 shared
        let a = admit(&p, 0, &pending);
        assert_eq!(a.flagship_admitted, 10);  // all flagship
        assert_eq!(a.shared_admitted, 85);    // 46 floor + 39 slack
        assert_eq!(a.admitted.len(), 95);
    }

    #[test]
    fn shared_underutilizes_slack_to_flagship() {
        let p = CapacityParams::default();
        let mut pending = Vec::new();
        for i in 0..110u8 { pending.push(job(i, true, 50)); }    // 110 flagship
        for i in 110..120u8 { pending.push(job(i, false, 50)); } // 10 shared
        let a = admit(&p, 0, &pending);
        assert_eq!(a.shared_admitted, 10);    // all shared
        assert_eq!(a.flagship_admitted, 85);  // 49 floor + 36 slack
        assert_eq!(a.admitted.len(), 95);
    }

    #[test]
    fn single_class_fills_available() {
        let p = CapacityParams::default(); // available 95
        let shared_only: Vec<PendingJob> = (0..120u8).map(|i| job(i, false, 50)).collect();
        let a = admit(&p, 0, &shared_only);
        assert_eq!(a.flagship_admitted, 0);
        assert_eq!(a.shared_admitted, 95);
        let flag_only: Vec<PendingJob> = (0..120u8).map(|i| job(i, true, 50)).collect();
        let b = admit(&p, 0, &flag_only);
        assert_eq!(b.flagship_admitted, 95);
        assert_eq!(b.shared_admitted, 0);
    }

    #[test]
    fn highest_priority_admitted_lowest_deferred() {
        // zero the reserve so available == total == 10 for clean math.
        let p = CapacityParams { total_slots: 10, reserve_floor_bps: 0, reserve_max_bps: 0, ..CapacityParams::default() };
        // 14 flagship jobs, priorities 1..14 (job id i -> priority i+1); available 10 ⇒ 10 admitted.
        let pending: Vec<PendingJob> = (0..14u8).map(|i| job(i, true, (i + 1) as u64)).collect();
        let a = admit(&p, 0, &pending);
        assert_eq!(a.available, 10);
        assert_eq!(a.flagship_admitted, 10);
        assert_eq!(a.deferred.len(), 4);
        // the 4 lowest priorities (1,2,3,4 ⇒ job ids 0,1,2,3) deferred; the highest (id 13) admitted.
        for id in [0u8, 1, 2, 3] {
            assert!(a.deferred.contains(&[id; 32]), "lowest priority deferred");
        }
        assert!(a.admitted.contains(&[13u8; 32]), "highest priority admitted");
    }

    #[test]
    fn conservation_admitted_union_deferred_equals_pending() {
        let p = CapacityParams::default();
        let pending: Vec<PendingJob> =
            (0..200u8).map(|i| job(i, i % 3 == 0, (i as u64 * 7) % 13)).collect();
        let a = admit(&p, 300, &pending); // 3% churn
        let mut all = a.admitted.clone();
        all.extend(a.deferred.clone());
        all.sort();
        let mut expected: Vec<[u8; 32]> = pending.iter().map(|j| j.job_id).collect();
        expected.sort();
        assert_eq!(all, expected, "every pending job admitted or deferred, none lost/duplicated");
        assert_eq!(a.admitted.len() as u32, a.flagship_admitted + a.shared_admitted);
    }

    #[test]
    fn zero_available_admits_nothing() {
        let p = CapacityParams { total_slots: 1, ..CapacityParams::default() }; // available 0
        let pending: Vec<PendingJob> = (0..5u8).map(|i| job(i, i % 2 == 0, 1)).collect();
        let a = admit(&p, 0, &pending);
        assert_eq!(a.available, 0);
        assert!(a.admitted.is_empty());
        assert_eq!(a.deferred.len(), 5);
    }

    #[test]
    fn deterministic_and_input_order_independent() {
        let p = CapacityParams::default();
        let pending: Vec<PendingJob> =
            (0..50u8).map(|i| job(i, i % 2 == 0, (i as u64 * 3) % 7)).collect();
        // pure: same input twice ⇒ identical Admission (incl. vector order)
        assert_eq!(admit(&p, 250, &pending), admit(&p, 250, &pending));
        // order-independent: the total-order sort key makes a reversed input yield the same result,
        // so two nodes that observe the block's jobs in different orders still agree.
        let mut reversed = pending.clone();
        reversed.reverse();
        assert_eq!(admit(&p, 250, &pending), admit(&p, 250, &reversed));
    }
}
