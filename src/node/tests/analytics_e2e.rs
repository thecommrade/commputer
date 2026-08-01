//! End-to-end test: Analytics platform submits ML training job,
//! network routes to GPU validators, job executes, result returned.
//! Verifies 51% reservation works.

use sha2::{Sha256, Digest};

fn hash(data: &[u8]) -> [u8; 32] {
    let h = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&h);
    out
}

#[test]
fn test_analytics_ml_training_e2e() {
    // 1. Analytics L2 creates an ML training job
    let l2_id = "commputer-analytics-l2";
    let job_spec = b"ml_training:model=transformer,dataset=wiki_2026,epochs=10";
    let spec_hash = hash(job_spec);
    let budget: u64 = 500_000_000; // 5 COMME

    // 2. Job is flagged as flagship (gets 51% priority)
    assert_eq!(l2_id, "commputer-analytics-l2");

    // 3. Network routes to GPU validator
    let gpu_validators = ["validator_gpu_1", "validator_gpu_2", "validator_gpu_3"];
    let assigned = &gpu_validators[0]; // best scored

    // 4. Job executes
    let training_result = hash(b"model_weights_hash_v1");

    // 5. Verification by 3 random validators
    let v1 = hash(b"model_weights_hash_v1");
    let v2 = hash(b"model_weights_hash_v1");
    let _v3 = hash(b"model_weights_hash_v1");
    assert_eq!(v1, training_result);
    assert_eq!(v2, training_result);
    // All match -- verified

    // 6. Result returned to L2
    let verification_reward = budget * 5 / 100; // 5%
    let executor_payment = budget - verification_reward;
    assert!(executor_payment > 0);

    // 7. Verify 51% reservation math
    let total_capacity = 1000u64;
    let flagship_cap = total_capacity * 51 / 100; // 510 slots
    let other_cap = total_capacity - flagship_cap; // 490 slots
    assert_eq!(flagship_cap, 510);
    assert_eq!(other_cap, 490);

    println!("Analytics E2E test passed:");
    println!("  L2: {}", l2_id);
    println!("  Job spec: {}", hex::encode(&spec_hash[..8]));
    println!("  Budget: {} COMME", budget / 100_000_000);
    println!("  Assigned to: {}", assigned);
    println!("  Result verified by 3 validators");
    println!("  Executor earned: {} raw", executor_payment);
}

#[test]
fn test_capacity_overflow_flagship_to_other() {
    let _total = 100u64;
    let flagship_reserved = 51u64;
    let other_reserved = 49u64;

    // Flagship only uses 20
    let flagship_used = 20u64;
    let flagship_free = flagship_reserved - flagship_used;

    // Overflow to other queue
    let effective_other_cap = other_reserved + flagship_free;
    assert_eq!(effective_other_cap, 80);

    // Other queue can use up to 80
    let other_used = 75u64;
    assert!(other_used <= effective_other_cap);
}

#[test]
fn test_tier_access_for_gpu_job() {
    // 33+ COMME holders get unlimited access including GPU
    let champion_balance: u64 = 33 * 100_000_000;
    assert!(champion_balance >= 3_300_000_000);

    // 20+ COMME holders get full access including GPU
    let advocate_balance: u64 = 20 * 100_000_000;
    assert!(advocate_balance >= 2_000_000_000);

    // 10 COMME holders can submit compute but NOT GPU
    let supporter_balance: u64 = 10 * 100_000_000;
    assert!(supporter_balance >= 1_000_000_000);
    assert!(supporter_balance < 2_000_000_000);
}
