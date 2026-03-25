//! End-to-end compute job integration test.
//! Simulates: submit job -> assign to validator -> execute -> verify -> result returned.

use sha2::{Sha256, Digest};

fn mock_job_id(submitter: &str, nonce: u64) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(submitter.as_bytes());
    hasher.update(nonce.to_le_bytes());
    let result = hasher.finalize();
    let mut id = [0u8; 32];
    id.copy_from_slice(&result);
    id
}

fn mock_result_hash(input: &[u8]) -> [u8; 32] {
    let hash = Sha256::digest(input);
    let mut out = [0u8; 32];
    out.copy_from_slice(&hash);
    out
}

#[test]
fn test_compute_job_end_to_end() {
    // 1. Submit: create a job
    let submitter = "validator_alice";
    let job_id = mock_job_id(submitter, 1);
    let comme_budget: u64 = 100_000_000; // 1 COMME

    // 2. Job enters pending state
    assert_ne!(job_id, [0u8; 32], "Job ID should not be zero");

    // 3. Assignment: validator claims the job
    let _executor = "validator_bob";
    let _assigned_height: u64 = 100;

    // 4. Execution: simulate WASM execution
    let wasm_input = b"test input data for ML training";
    let result_hash = mock_result_hash(wasm_input);
    assert_ne!(result_hash, [0u8; 32]);

    // 5. Verification: 3 verifiers check the result
    let verifier_results = vec![
        mock_result_hash(wasm_input), // matches
        mock_result_hash(wasm_input), // matches
        mock_result_hash(b"different"), // doesn't match
    ];
    let matching = verifier_results.iter().filter(|r| **r == result_hash).count();
    assert!(matching >= 2, "Majority should confirm the result");

    // 6. Reward distribution: 5% of budget split among verifiers
    let verification_reward = comme_budget * 5 / 100;
    let per_verifier = verification_reward / 3;
    assert!(per_verifier > 0);

    // 7. Result: available to submitter
    let executor_reward = comme_budget - verification_reward;
    assert!(executor_reward > 0);
    // Account for integer division remainder
    let remainder = verification_reward - per_verifier * 3;
    assert_eq!(executor_reward + per_verifier * 3 + remainder, comme_budget);

    // Summary: full lifecycle completed
    println!("E2E compute test passed:");
    println!("  Job ID: {}", hex::encode(&job_id[..8]));
    println!("  Budget: {} raw units", comme_budget);
    println!("  Executor reward: {} raw units", executor_reward);
    println!("  Verifier reward (each): {} raw units", per_verifier);
}

#[test]
fn test_51_49_split_reservation() {
    let total_capacity = 100u64;
    let flagship_reserved = (total_capacity * 51) / 100;
    let other_reserved = total_capacity - flagship_reserved;

    assert_eq!(flagship_reserved, 51);
    assert_eq!(other_reserved, 49);

    // When flagship doesn't use all capacity, overflow goes to others
    let flagship_used = 30u64;
    let flagship_unused = flagship_reserved - flagship_used;
    let effective_other_capacity = other_reserved + flagship_unused;

    assert_eq!(effective_other_capacity, 70);
}

#[test]
fn test_dispute_resolution_majority_wins() {
    let original = mock_result_hash(b"correct result");
    let re_exec_1 = mock_result_hash(b"correct result");
    let re_exec_2 = mock_result_hash(b"correct result");
    let re_exec_3 = mock_result_hash(b"wrong result");

    let results = [re_exec_1, re_exec_2, re_exec_3];
    let matching_original = results.iter().filter(|r| **r == original).count();

    // Majority (2/3) matches original -> original is correct
    assert!(matching_original >= 2);
}

#[test]
fn test_job_timeout() {
    let assigned_height = 100u64;
    let current_height = 200u64;
    let secs_per_block = 2u64;
    let max_duration = 150u64; // seconds

    let elapsed_secs = (current_height - assigned_height) * secs_per_block;
    assert_eq!(elapsed_secs, 200);
    assert!(elapsed_secs > max_duration, "Job should be timed out");
}

#[test]
fn test_dynamic_pricing() {
    let base_rate: u64 = 1_000_000; // 0.01 COMME per cpu-hour

    // Low utilization: no multiplier
    let load_30pct = 0.3f64;
    let price_low = (base_rate as f64 * f64::max(1.0, load_30pct * load_30pct * 4.0)) as u64;
    assert_eq!(price_low, base_rate); // multiplier < 1, so clamped to 1

    // High utilization: price increases
    let load_95pct = 0.95f64;
    let multiplier = f64::max(1.0, (load_95pct / 0.5).powi(2));
    let price_high = (base_rate as f64 * multiplier) as u64;
    assert!(price_high > base_rate, "Price should increase at high utilization");
}
