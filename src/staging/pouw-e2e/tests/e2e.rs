use commputer_pouw_e2e::scenarios::{self, Terminal};
use commputer_pouw::job::Verdict;
use commputer_pouw::economics::EconViolation;

#[test]
fn scenario_1_happy_path() {
    let r = scenarios::happy_path();
    assert_eq!(r.effective, 30);
    assert_eq!(r.abstained, 0);
    assert!(r.program_present);
    match r.terminal {
        Terminal::Settled(Verdict::Confirmed { .. }, out) => {
            assert_eq!((out.worker_paid, out.verifiers_paid, out.burned), (3_366, 396, 198));
        }
        other => panic!("expected Settled/Confirmed, got {other:?}"),
    }
    assert!(r.conserved);
}

#[test]
fn scenario_2_cheating_executor_caught() {
    let r = scenarios::cheating_executor();
    assert!(r.program_present && r.effective == 30);
    match r.terminal {
        Terminal::Settled(Verdict::Disputed { .. }, out) => {
            assert_eq!(out.submitter_refunded, 3_960, "full budget refunded");
            assert!(out.slashed.iter().any(|(_, amt)| *amt > 0), "executor bond slashed");
        }
        other => panic!("expected Settled/Disputed, got {other:?}"),
    }
    assert!(r.conserved);
}

#[test]
fn scenario_3_underfunded_rejected_before_da() {
    let r = scenarios::underfunded();
    // The DA gate was never run (no fetch, no execution).
    assert_eq!(r.effective, 0);
    assert_eq!(r.abstained, 0);
    assert!(!r.program_present);
    assert!(matches!(r.terminal, Terminal::Rejected(EconViolation::BudgetBelowMin { .. })));
    assert!(r.conserved, "nothing moved");
}

#[test]
fn scenario_4_partial_withholding_partial_abstain() {
    let r = scenarios::partial_withholding();
    assert!(r.abstained >= 1, "some verifiers sampled a withheld chunk and abstained");
    assert!(r.effective >= 3, "enough survivors to form a committee (k=3)");
    assert!(r.program_present, "survivors reconstructed the program");
    match r.terminal {
        Terminal::Settled(Verdict::Confirmed { .. }, _) => {}
        other => panic!("expected Settled/Confirmed on survivors, got {other:?}"),
    }
    assert!(r.conserved);
}

#[test]
fn scenario_5_erroring_guest_settles_confirmed() {
    let r = scenarios::erroring_guest();
    assert!(r.program_present && r.effective == 30);
    // The happy path settles on the hash of DOUBLER's real output; the erroring guest must
    // settle on a DIFFERENT value — the WASM error sentinel — proving the trap path was reached
    // and folded end-to-end, not that the guest produced some normal output.
    let happy_hash = match scenarios::happy_path().terminal {
        Terminal::Settled(Verdict::Confirmed { result_hash }, _) => result_hash,
        other => panic!("happy path must settle Confirmed, got {other:?}"),
    };
    match r.terminal {
        // Real WASM trap → error sentinel; honest executor + committee agree → Confirmed,
        // executor paid the worker share (founder-locked error-outcome policy).
        Terminal::Settled(Verdict::Confirmed { result_hash }, out) => {
            assert_ne!(result_hash, happy_hash, "must settle on the error sentinel, not normal output");
            assert_eq!((out.worker_paid, out.verifiers_paid, out.burned), (3_366, 396, 198));
        }
        other => panic!("expected Settled/Confirmed, got {other:?}"),
    }
    assert!(r.conserved);
}

#[test]
fn scenario_6_total_withholding_short_circuits() {
    let r = scenarios::total_withholding();
    assert_eq!(r.effective, 0);
    assert!(!r.program_present);
    assert!(matches!(r.terminal, Terminal::NoCommittee));
    assert!(r.conserved);
}

#[test]
fn scenario_7_tampered_publish_rebind_abstains() {
    let r = scenarios::tampered_publish();
    assert_eq!(r.effective, 0, "every candidate abstains at the sha256 re-bind");
    assert!(!r.program_present);
    assert!(matches!(r.terminal, Terminal::NoCommittee));
    assert!(r.conserved);
}
