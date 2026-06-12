//! `pouw-sim` — the Monte-Carlo tournament entry point (plan Task 14).
//!
//! WHAT THIS DOES: runs the default tournament at the tuned `safe_regime` and prints a
//! small, founder-readable metrics table (strategy, plays, caught %, net EV/play) plus a
//! one-line verdict. The table is the human-facing deliverable: it shows, on real money
//! moved through the production settlement branches, that honest play dominates — every
//! modeled cheating strategy is EV-negative — at the documented safe parameter regime.
//!
//! HOW TO RUN: `cargo run -p commputer-pouw --bin pouw-sim`.
//! HOW TO READ: see `src/staging/pouw/README.md` (Task 15).
//!
//! The machine-checked version of the same claim is the unit test
//! `tournament::tests::honest_play_dominates_at_safe_regime`, run by
//! `cargo test -p commputer-pouw`.

/// Adversarial agent strategies (Task 13): the executor/verifier strategies the
/// tournament pits against each other.
mod agents;
/// Monte-Carlo tournament + the per-strategy EV proof (Task 14).
mod tournament;
#[cfg(feature = "wasm-runtime")]
mod realfuel;

use tournament::{run_tournament, safe_regime};

/// Number of seeded jobs the default tournament plays. Large enough that the per-strategy
/// EV averages are stable across seeds (the unit test uses the same scale).
const DEFAULT_JOBS: u64 = 50_000;
/// Fixed seed so `pouw-sim` prints the same table on every run (reproducible deliverable).
const DEFAULT_SEED: u64 = 0xC0FFEE;

fn main() {
    let (params, costs) = safe_regime();
    let report = run_tournament(DEFAULT_JOBS, &params, &costs, DEFAULT_SEED);
    print!("{}", report.table());

    #[cfg(feature = "wasm-runtime")]
    {
        let classes = realfuel::measure_classes();
        println!("\n=== REAL-FUEL ECONOMICS (fuel-economics spec §5) ===");
        for c in &classes {
            println!("class {:>6}: measured fuel {:>12}", c.name, c.measured_fuel);
        }
        let results = realfuel::run_sweep(&realfuel::default_grid(), &classes[0], 4_000, DEFAULT_SEED);
        print!("{}", realfuel::table(&results));
    }
}
