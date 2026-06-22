//! Read-only lifecycle trace: `e2e-trace [scenario]` (default: happy). Deterministic;
//! prints only to stdout; exits 1 iff conservation fails. Shares scenarios.rs with the tests.

use commputer_pouw_e2e::scenarios::{self, Terminal};

fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "happy".to_string());
    let r = scenarios::run(&name);
    let t = &r.trace;

    println!("SCENARIO  {}", r.name);
    println!(
        "PUBLISH   program_id={} da_root={} N={} 2N={} chunk_size={}",
        t.program_id8, t.da_root8, t.n_data, t.n_total, t.chunk_size
    );
    println!(
        "GATE      candidates={} available={} abstained={} reconstructed={}",
        r.effective + r.abstained, r.effective, r.abstained,
        if r.program_present { "yes" } else { "no" }
    );
    println!("EXECUTE   oracle=wasmi-1.0.9 fuel_cap={}", t.fuel_cap);
    match &r.terminal {
        Terminal::Settled(v, o) => {
            println!("VERDICT   {v:?}");
            println!(
                "SETTLE    worker={} verifiers={} burned={} refunded={} slashed={:?}",
                o.worker_paid, o.verifiers_paid, o.burned, o.submitter_refunded, o.slashed
            );
        }
        Terminal::Rejected(e) => println!("VERDICT   Rejected({e:?})"),
        Terminal::NoCommittee => {
            println!("VERDICT   NoCommittee (short-circuit: insufficient data-holding verifiers)")
        }
    }
    println!("CONSERVE  {}", if r.conserved { "PASS" } else { "FAIL" });
    if !r.conserved {
        std::process::exit(1);
    }
}
