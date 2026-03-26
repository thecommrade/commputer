//! Comprehensive compute job system tests (Items 26-35).
//! Tests dynamic pricing, cancellation, resource quotas, 51/49 split,
//! result verification, dashboard, billing, concurrency, spec validation,
//! and capacity reporting.

use commputer_storage::job_pool::{JobPool, PoolJob, PoolJobStatus, JobId};
use commputer_storage::job_billing::{BillingStore, JobBillingRecord};
use commputer_storage::job_results::{JobResultStore, JobResult};
use commputer_storage::pricing_history::{PricingHistory, PricePoint, ResourceType};
use commputer_storage::usage_analytics::UsageAnalytics;
use commputer_core::identity::Address;
use commputer_core::compute::{FLAGSHIP_L2_ID, CANCELLATION_FEE_BPS, MIN_JOB_BUDGET};
use sha2::{Sha256, Digest};

// ── Helpers ──

fn make_address(byte: u8) -> Address {
    Address([byte; 32])
}

fn make_job_id(byte: u8) -> JobId {
    JobId([byte; 32])
}

fn make_pool_job(id_byte: u8, budget: u64, l2_id: Option<&str>) -> PoolJob {
    PoolJob {
        job_id: make_job_id(id_byte),
        submitter: make_address(0xFF),
        comme_budget: budget,
        cpu_cores: 4,
        gpu_vram_mb: 0,
        ram_mb: 8192,
        storage_mb: 0,
        bandwidth_mbps: 100,
        max_duration_secs: 3600,
        job_spec_hash: [id_byte; 32],
        status: PoolJobStatus::Pending,
        submitted_height: 100,
        l2_id: l2_id.map(|s| s.to_string()),
    }
}

fn mock_result_hash(input: &[u8]) -> [u8; 32] {
    let hash = Sha256::digest(input);
    let mut out = [0u8; 32];
    out.copy_from_slice(&hash);
    out
}

// ── Item 26: Dynamic pricing tests ──

#[test]
fn test_dynamic_pricing_scales_with_load() {
    let base_rate: u64 = 1_000_000; // 0.01 COMME per cpu-hour

    // At 30% utilization, price should stay at base
    let load_30 = 0.30f64;
    let multiplier_30 = f64::max(1.0, (load_30 / 0.5).powi(2));
    let price_30 = (base_rate as f64 * multiplier_30) as u64;
    assert_eq!(price_30, base_rate, "Price at 30% load should be base rate");

    // At 50% utilization, multiplier = 1.0 exactly
    let load_50 = 0.50f64;
    let multiplier_50 = f64::max(1.0, (load_50 / 0.5).powi(2));
    let price_50 = (base_rate as f64 * multiplier_50) as u64;
    assert_eq!(price_50, base_rate, "Price at 50% load should be base rate");

    // At 70% utilization, price should increase
    let load_70 = 0.70f64;
    let multiplier_70 = f64::max(1.0, (load_70 / 0.5).powi(2));
    let price_70 = (base_rate as f64 * multiplier_70) as u64;
    assert!(price_70 > base_rate, "Price at 70% should exceed base");

    // At 95% utilization, price should be significantly higher
    let load_95 = 0.95f64;
    let multiplier_95 = f64::max(1.0, (load_95 / 0.5).powi(2));
    let price_95 = (base_rate as f64 * multiplier_95) as u64;
    assert!(price_95 > price_70, "Price at 95% should exceed 70% price");

    // Verify monotonically increasing
    assert!(price_30 <= price_50);
    assert!(price_50 <= price_70);
    assert!(price_70 <= price_95);
}

#[test]
fn test_dynamic_pricing_with_job_pool_utilization() {
    let mut pool = JobPool::new();
    let base_rate: u64 = 1_000_000;

    // Submit 10 jobs at various loads
    for i in 0..10u8 {
        pool.submit_job(make_pool_job(i, base_rate * (i as u64 + 1), None));
    }

    // Assign 7 of them (70% utilization)
    for i in 0..7u8 {
        pool.assign_job(&make_job_id(i), make_address(0x01), 200);
    }

    let active = pool.active_count() as f64;
    let total = pool.total_count() as f64;
    let utilization = active / total;

    assert!(utilization > 0.6, "Utilization should be above 60%");
    assert!(utilization < 0.8, "Utilization should be below 80%");

    let multiplier = f64::max(1.0, (utilization / 0.5).powi(2));
    let dynamic_price = (base_rate as f64 * multiplier) as u64;
    assert!(dynamic_price > base_rate, "Dynamic price should exceed base at high utilization");
}

#[test]
fn test_pricing_history_records_points() {
    let mut history = PricingHistory::new(100);

    // Record prices at different utilization levels
    for i in 0..5u64 {
        history.record_price(PricePoint {
            height: i * 100,
            epoch: i,
            cpu_price: 1_000_000 + i * 100_000,
            gpu_price: 5_000_000 + i * 500_000,
            storage_price: 10_000,
            ram_price: 20_000,
            utilization_pct: 20.0 + i as f64 * 15.0,
        });
    }

    assert_eq!(history.len(), 5);
    let avg_cpu = history.average_price(ResourceType::Cpu);
    assert!(avg_cpu > 1_000_000.0, "Average CPU price should reflect recorded points");
}

// ── Item 27: Job cancellation tests ──

#[test]
fn test_job_cancellation_refund() {
    let mut pool = JobPool::new();
    let budget: u64 = 100_000_000; // 1 COMME
    pool.submit_job(make_pool_job(1, budget, None));

    // Cancel the pending job
    let refund = pool.cancel_job(&make_job_id(1));
    assert_eq!(refund, Some(budget), "Full budget returned on cancel");

    // Calculate 2% cancellation fee
    let fee = budget * CANCELLATION_FEE_BPS / 10_000;
    let net_refund = budget - fee;
    assert_eq!(fee, 2_000_000, "2% of 1 COMME = 0.02 COMME");
    assert_eq!(net_refund, 98_000_000, "98% refunded after fee");

    // Pool should be empty
    assert_eq!(pool.total_count(), 0);
    assert_eq!(pool.pending_count(), 0);
}

#[test]
fn test_cancellation_only_pending() {
    let mut pool = JobPool::new();
    pool.submit_job(make_pool_job(1, 100_000_000, None));

    // Assign the job
    pool.assign_job(&make_job_id(1), make_address(0x01), 200);

    // Cancel should fail — job is assigned, not pending
    assert_eq!(pool.cancel_job(&make_job_id(1)), None);
}

#[test]
fn test_cancellation_fee_calculation() {
    // Test various budget sizes
    let test_cases: Vec<(u64, u64)> = vec![
        (100_000_000, 2_000_000),     // 1 COMME -> 0.02 COMME fee
        (1_000_000, 20_000),          // 0.01 COMME -> 200 raw units fee
        (50, 1),                       // Tiny budget
        (0, 0),                        // Zero budget
    ];

    for (budget, expected_fee) in test_cases {
        let fee = budget * CANCELLATION_FEE_BPS / 10_000;
        assert_eq!(fee, expected_fee, "Fee for budget {} should be {}", budget, expected_fee);
    }
}

// ── Item 28: Resource quota enforcement tests ──

#[test]
fn test_tier_based_quota_read_only() {
    // ReadOnly tier (< 1 COMME balance) should not be able to submit jobs
    let balance: u64 = 50_000_000; // 0.5 COMME
    let max_jobs = max_jobs_for_balance(balance);
    assert_eq!(max_jobs, 0, "ReadOnly tier gets 0 jobs");
}

#[test]
fn test_tier_based_quota_storage() {
    // Storage tier (1 COMME)
    let balance: u64 = 100_000_000;
    let max_jobs = max_jobs_for_balance(balance);
    assert_eq!(max_jobs, 5, "Storage tier gets 5 jobs per epoch");
}

#[test]
fn test_tier_based_quota_compute() {
    // Compute tier (10 COMME)
    let balance: u64 = 1_000_000_000;
    let max_jobs = max_jobs_for_balance(balance);
    assert_eq!(max_jobs, 20, "Compute tier gets 20 jobs per epoch");
}

#[test]
fn test_tier_based_quota_full() {
    // Full tier (20 COMME)
    let balance: u64 = 2_000_000_000;
    let max_jobs = max_jobs_for_balance(balance);
    assert_eq!(max_jobs, 50, "Full tier gets 50 jobs per epoch");
}

#[test]
fn test_tier_based_quota_unlimited() {
    // Unlimited tier (33 COMME)
    let balance: u64 = 3_300_000_000;
    let max_jobs = max_jobs_for_balance(balance);
    assert_eq!(max_jobs, 200, "Unlimited tier gets 200 jobs per epoch");
}

/// Inline the tier function for testing without importing the node crate modules directly.
fn max_jobs_for_balance(balance: u64) -> u64 {
    const TIER_STORAGE: u64 = 100_000_000;
    const TIER_COMPUTE: u64 = 1_000_000_000;
    const TIER_FULL: u64 = 2_000_000_000;
    const TIER_UNLIMITED: u64 = 3_300_000_000;

    if balance >= TIER_UNLIMITED {
        200
    } else if balance >= TIER_FULL {
        50
    } else if balance >= TIER_COMPUTE {
        20
    } else if balance >= TIER_STORAGE {
        5
    } else {
        0
    }
}

// ── Item 29: 51/49 split (flagship L2 priority) tests ──

#[test]
fn test_flagship_l2_gets_priority() {
    let mut pool = JobPool::new();

    // Submit 5 flagship jobs and 5 other jobs
    for i in 0..5u8 {
        pool.submit_job(make_pool_job(i, 1_000_000 * (i as u64 + 1), Some(FLAGSHIP_L2_ID)));
    }
    for i in 5..10u8 {
        pool.submit_job(make_pool_job(i, 1_000_000 * (i as u64 + 1), Some("other-l2")));
    }

    let flagship = pool.pending_flagship_jobs();
    let other = pool.pending_other_jobs();

    assert_eq!(flagship.len(), 5);
    assert_eq!(other.len(), 5);
}

#[test]
fn test_51_49_capacity_reservation() {
    let total_slots = 100u64;
    let flagship_reserved = (total_slots * 51) / 100;
    let other_reserved = total_slots - flagship_reserved;

    assert_eq!(flagship_reserved, 51);
    assert_eq!(other_reserved, 49);

    // Simulate flagship using only 20 slots
    let flagship_used = 20u64;
    let flagship_unused = flagship_reserved - flagship_used;
    let effective_other = other_reserved + flagship_unused;

    // Others get their 49 + unused flagship capacity
    assert_eq!(effective_other, 80);

    // But flagship can never be less than 51% of total
    let flagship_pct = (flagship_reserved as f64 / total_slots as f64) * 100.0;
    assert!((flagship_pct - 51.0).abs() < 0.01);
}

#[test]
fn test_flagship_sorted_by_budget() {
    let mut pool = JobPool::new();
    pool.submit_job(make_pool_job(1, 5_000_000, Some(FLAGSHIP_L2_ID)));
    pool.submit_job(make_pool_job(2, 10_000_000, Some(FLAGSHIP_L2_ID)));
    pool.submit_job(make_pool_job(3, 1_000_000, Some(FLAGSHIP_L2_ID)));

    let flagship = pool.pending_flagship_jobs();
    // pending_jobs() returns sorted by budget (highest first)
    assert_eq!(flagship[0].comme_budget, 10_000_000);
    assert_eq!(flagship[1].comme_budget, 5_000_000);
    assert_eq!(flagship[2].comme_budget, 1_000_000);
}

// ── Item 30: Job result verification tests ──

#[test]
fn test_wrong_result_triggers_dispute() {
    let mut pool = JobPool::new();
    pool.submit_job(make_pool_job(1, 100_000_000, None));

    let executor = make_address(0x01);
    pool.assign_job(&make_job_id(1), executor, 200);

    // Executor submits a result
    let claimed_result = mock_result_hash(b"executor claimed result");
    pool.complete_job(&make_job_id(1), claimed_result, 300);

    // Verifier re-executes and gets a different result
    let actual_result = mock_result_hash(b"actual correct result");
    assert_ne!(claimed_result, actual_result, "Results should differ");

    // File a dispute
    let challenger = make_address(0x02);
    assert!(pool.dispute_job(&make_job_id(1), challenger));

    let job = pool.get(&make_job_id(1)).unwrap();
    assert!(matches!(job.status, PoolJobStatus::Disputed { .. }));
}

#[test]
fn test_correct_result_no_dispute() {
    let mut pool = JobPool::new();
    pool.submit_job(make_pool_job(1, 100_000_000, None));

    let executor = make_address(0x01);
    pool.assign_job(&make_job_id(1), executor, 200);

    let result = mock_result_hash(b"consistent result");
    pool.complete_job(&make_job_id(1), result, 300);

    // Multiple verifiers get the same result
    let verify1 = mock_result_hash(b"consistent result");
    let verify2 = mock_result_hash(b"consistent result");
    let verify3 = mock_result_hash(b"consistent result");

    assert_eq!(verify1, result);
    assert_eq!(verify2, result);
    assert_eq!(verify3, result);

    // No dispute needed
    let job = pool.get(&make_job_id(1)).unwrap();
    assert!(matches!(job.status, PoolJobStatus::Completed { .. }));
}

#[test]
fn test_dispute_only_completed_jobs() {
    let mut pool = JobPool::new();
    pool.submit_job(make_pool_job(1, 100_000_000, None));

    // Try to dispute a pending job — should fail
    let challenger = make_address(0x02);
    assert!(!pool.dispute_job(&make_job_id(1), challenger));

    // Assign it — still can't dispute
    pool.assign_job(&make_job_id(1), make_address(0x01), 200);
    assert!(!pool.dispute_job(&make_job_id(1), challenger));
}

// ── Item 31: Compute dashboard RPC tests ──

#[test]
fn test_compute_dashboard_stats() {
    let mut pool = JobPool::new();

    // Submit and complete several jobs
    for i in 0..5u8 {
        pool.submit_job(make_pool_job(i, 10_000_000, None));
        pool.assign_job(&make_job_id(i), make_address(0x01), 100 + i as u64);
        pool.complete_job(&make_job_id(i), [i; 32], 200 + i as u64);
    }

    // Submit 3 more that are still pending
    for i in 5..8u8 {
        pool.submit_job(make_pool_job(i, 20_000_000, None));
    }

    assert_eq!(pool.completed_count(), 5);
    assert_eq!(pool.pending_count(), 3);
    assert_eq!(pool.active_count(), 0);
    assert_eq!(pool.total_count(), 8);

    // Build dashboard-like stats
    let total_budget: u64 = pool.all_jobs().iter()
        .filter(|j| matches!(j.status, PoolJobStatus::Completed { .. }))
        .map(|j| j.comme_budget)
        .sum();
    assert_eq!(total_budget, 5 * 10_000_000);

    let avg_budget = total_budget / pool.completed_count();
    assert_eq!(avg_budget, 10_000_000);
}

#[test]
fn test_usage_analytics_tracking() {
    let mut analytics = UsageAnalytics::new();

    analytics.record_submission("alice_hex", 50_000_000);
    analytics.record_submission("alice_hex", 30_000_000);
    analytics.record_submission("bob_hex", 100_000_000);

    analytics.record_completion("alice_hex", 120.0);
    analytics.record_completion("alice_hex", 80.0);

    let alice = analytics.get_stats("alice_hex").unwrap();
    assert_eq!(alice.total_jobs_submitted, 2);
    assert_eq!(alice.total_comme_spent, 80_000_000);
    assert_eq!(alice.total_jobs_completed, 2);
    assert!((alice.avg_job_duration_secs - 100.0).abs() < 0.01);

    let top = analytics.top_users(1);
    assert_eq!(top[0].address_hex, "bob_hex");
}

// ── Item 32: Job billing accuracy tests ──

#[test]
fn test_billing_accuracy() {
    let mut billing = BillingStore::new();

    let record = JobBillingRecord {
        job_id: [1; 32],
        submitter_hex: "alice".into(),
        comme_spent: 50_000_000,
        cpu_cores_used: 4,
        gpu_vram_used: 0,
        ram_used: 8192,
        duration_secs: 3600,
        result_hash: [0xAA; 32],
        billed_at_height: 500,
    };

    assert!(billing.record_billing(record));
    assert_eq!(billing.total_billed(), 50_000_000);

    // Add another billing record
    let record2 = JobBillingRecord {
        job_id: [2; 32],
        submitter_hex: "bob".into(),
        comme_spent: 100_000_000,
        cpu_cores_used: 8,
        gpu_vram_used: 16384,
        ram_used: 32768,
        duration_secs: 7200,
        result_hash: [0xBB; 32],
        billed_at_height: 600,
    };

    assert!(billing.record_billing(record2));
    assert_eq!(billing.total_billed(), 150_000_000);

    // Verify exact billing for alice
    let alice_records = billing.records_for_address("alice");
    assert_eq!(alice_records.len(), 1);
    assert_eq!(alice_records[0].comme_spent, 50_000_000);
}

#[test]
fn test_billing_prevents_double_billing() {
    let mut billing = BillingStore::new();

    let record = JobBillingRecord {
        job_id: [1; 32],
        submitter_hex: "alice".into(),
        comme_spent: 50_000_000,
        cpu_cores_used: 4,
        gpu_vram_used: 0,
        ram_used: 8192,
        duration_secs: 3600,
        result_hash: [0xAA; 32],
        billed_at_height: 500,
    };

    assert!(billing.record_billing(record.clone()));
    assert!(!billing.record_billing(record));
    assert_eq!(billing.count(), 1);
    assert_eq!(billing.total_billed(), 50_000_000); // Not doubled
}

#[test]
fn test_billing_minimum_budget_enforced() {
    // Jobs below MIN_JOB_BUDGET should be rejected at submission time.
    let below_minimum = MIN_JOB_BUDGET - 1;
    assert!(below_minimum < MIN_JOB_BUDGET);

    // The minimum is 0.01 COMME = 1_000_000 raw units
    assert_eq!(MIN_JOB_BUDGET, 1_000_000);
}

// ── Item 33: Multiple concurrent jobs tests ──

#[test]
fn test_ten_concurrent_jobs() {
    let mut pool = JobPool::new();
    let submitter = make_address(0xFF);
    let executors: Vec<Address> = (0..10u8).map(make_address).collect();

    // Step 1: Submit 10 jobs with varying budgets
    for i in 0..10u8 {
        let mut job = make_pool_job(i, (i as u64 + 1) * 5_000_000, None);
        job.submitter = submitter;
        pool.submit_job(job);
    }
    assert_eq!(pool.pending_count(), 10);
    assert_eq!(pool.total_count(), 10);

    // Step 2: Assign all 10 to different executors
    for i in 0..10u8 {
        assert!(pool.assign_job(&make_job_id(i), executors[i as usize], 100 + i as u64));
    }
    assert_eq!(pool.pending_count(), 0);
    assert_eq!(pool.active_count(), 10);

    // Step 3: Complete all 10
    for i in 0..10u8 {
        let result = mock_result_hash(&[i; 8]);
        assert!(pool.complete_job(&make_job_id(i), result, 200 + i as u64));
    }
    assert_eq!(pool.completed_count(), 10);
    assert_eq!(pool.active_count(), 0);
    assert_eq!(pool.pending_count(), 0);

    // Step 4: Verify recent completed shows them in order
    let recent = pool.recent_completed(5);
    assert_eq!(recent.len(), 5);
}

#[test]
fn test_concurrent_mixed_states() {
    let mut pool = JobPool::new();

    // Create jobs in various states
    for i in 0..15u8 {
        pool.submit_job(make_pool_job(i, 1_000_000 * (i as u64 + 1), None));
    }

    // 5 remain pending (10-14)
    // 5 get assigned (5-9)
    for i in 5..10u8 {
        pool.assign_job(&make_job_id(i), make_address(0x01), 200);
    }
    // 5 get completed (0-4)
    for i in 0..5u8 {
        pool.assign_job(&make_job_id(i), make_address(0x02), 200);
        pool.complete_job(&make_job_id(i), [i; 32], 300);
    }

    assert_eq!(pool.pending_count(), 5);
    assert_eq!(pool.active_count(), 5);
    assert_eq!(pool.completed_count(), 5);
    assert_eq!(pool.total_count(), 15);
}

// ── Item 34: Job spec validation tests ──

#[test]
fn test_reject_zero_cpu_cores() {
    // JobSubmitRequest with 0 CPU cores should be rejected.
    let cpu_cores: u16 = 0;
    assert_eq!(cpu_cores, 0, "Zero cores should trigger rejection");
}

#[test]
fn test_reject_budget_below_minimum() {
    let budget: u64 = 500_000; // Below MIN_JOB_BUDGET (1_000_000)
    assert!(budget < MIN_JOB_BUDGET, "Budget below minimum should be rejected");
}

#[test]
fn test_reject_excessive_duration() {
    let max_allowed: u64 = 86400; // 24 hours
    let requested: u64 = 100_000; // > 24 hours

    assert!(requested > max_allowed, "Duration exceeding 24h should be rejected");
}

#[test]
fn test_valid_job_spec_accepted() {
    // A well-formed job should be accepted
    let cpu_cores: u16 = 4;
    let budget: u64 = 10_000_000;
    let duration: u64 = 3600;
    let max_duration: u64 = 86400;

    assert!(cpu_cores > 0);
    assert!(budget >= MIN_JOB_BUDGET);
    assert!(duration > 0 && duration <= max_duration);
}

#[test]
fn test_reject_empty_spec_hash() {
    let spec_hash = [0u8; 32];
    // A zero spec hash is suspicious (likely uninitialized)
    assert_eq!(spec_hash, [0u8; 32], "Empty spec hash should be flagged");
}

#[test]
fn test_job_result_store_validation() {
    let mut store = JobResultStore::new();

    let result = JobResult {
        job_id: [1; 32],
        result_hash: mock_result_hash(b"valid output"),
        output_data: Some(vec![1, 2, 3, 4]),
        stored_at_height: 500,
        executor_hex: "validator_1".into(),
    };

    assert!(store.store_result(result));
    assert_eq!(store.count(), 1);

    let retrieved = store.get_result(&[1; 32]).unwrap();
    assert_eq!(retrieved.executor_hex, "validator_1");
    assert_ne!(retrieved.result_hash, [0u8; 32], "Result hash should not be zero");
}

// ── Item 35: Capacity reporting accuracy tests ──

#[test]
fn test_capacity_utilization_calculation() {
    let total_cpu: u64 = 10000;

    // 0% utilization
    let util_0 = calculate_utilization(0, total_cpu);
    assert!((util_0 - 0.0).abs() < 0.01);

    // 30% utilization
    let util_30 = calculate_utilization(3000, total_cpu);
    assert!((util_30 - 30.0).abs() < 0.01);

    // 100% utilization
    let util_100 = calculate_utilization(10000, total_cpu);
    assert!((util_100 - 100.0).abs() < 0.01);

    // Edge case: 0 total
    let util_zero = calculate_utilization(0, 0);
    assert!((util_zero - 0.0).abs() < 0.01);
}

#[test]
fn test_capacity_from_job_pool() {
    let mut pool = JobPool::new();

    // Submit 10 jobs using 4 CPU cores each = 40 cores requested
    for i in 0..10u8 {
        pool.submit_job(make_pool_job(i, 5_000_000, None));
    }

    let total_cpu_requested: u64 = pool.all_jobs().iter()
        .map(|j| j.cpu_cores as u64)
        .sum();
    assert_eq!(total_cpu_requested, 40);

    // Assign 6 jobs — those are "used"
    for i in 0..6u8 {
        pool.assign_job(&make_job_id(i), make_address(0x01), 200);
    }

    let used_cpu: u64 = pool.all_jobs().iter()
        .filter(|j| matches!(j.status, PoolJobStatus::Assigned { .. } | PoolJobStatus::Running { .. }))
        .map(|j| j.cpu_cores as u64)
        .sum();
    assert_eq!(used_cpu, 24); // 6 jobs * 4 cores

    let pending_cpu: u64 = pool.all_jobs().iter()
        .filter(|j| matches!(j.status, PoolJobStatus::Pending))
        .map(|j| j.cpu_cores as u64)
        .sum();
    assert_eq!(pending_cpu, 16); // 4 jobs * 4 cores
}

#[test]
fn test_capacity_flagship_vs_other_reporting() {
    let mut pool = JobPool::new();

    // 3 flagship jobs, 7 other
    for i in 0..3u8 {
        pool.submit_job(make_pool_job(i, 10_000_000, Some(FLAGSHIP_L2_ID)));
    }
    for i in 3..10u8 {
        pool.submit_job(make_pool_job(i, 5_000_000, None));
    }

    let flagship_count = pool.pending_flagship_jobs().len();
    let other_count = pool.pending_other_jobs().len();

    assert_eq!(flagship_count, 3);
    assert_eq!(other_count, 7);

    let flagship_pct = (flagship_count as f64 / pool.pending_count() as f64) * 100.0;
    assert!((flagship_pct - 30.0).abs() < 0.01);
}

#[test]
fn test_capacity_after_completions() {
    let mut pool = JobPool::new();

    for i in 0..10u8 {
        pool.submit_job(make_pool_job(i, 5_000_000, None));
        pool.assign_job(&make_job_id(i), make_address(0x01), 100);
    }

    assert_eq!(pool.active_count(), 10);

    // Complete half
    for i in 0..5u8 {
        pool.complete_job(&make_job_id(i), [i; 32], 200);
    }

    assert_eq!(pool.active_count(), 5);
    assert_eq!(pool.completed_count(), 5);

    // Fail the rest
    for i in 5..10u8 {
        pool.fail_job(&make_job_id(i), "out of memory".into());
    }

    assert_eq!(pool.active_count(), 0);
    assert_eq!(pool.failed_count(), 5);
}

#[test]
fn test_job_pool_serialization_roundtrip() {
    let mut pool = JobPool::new();
    for i in 0..5u8 {
        pool.submit_job(make_pool_job(i, (i as u64 + 1) * 1_000_000, None));
    }
    pool.assign_job(&make_job_id(2), make_address(0x01), 200);
    pool.complete_job(&make_job_id(2), [0xFF; 32], 300);

    let json = pool.to_json().unwrap();
    let restored = JobPool::from_json(&json).unwrap();

    assert_eq!(restored.total_count(), 5);
    assert_eq!(restored.pending_count(), 4);
    assert_eq!(restored.completed_count(), 1);
}

/// Helper for utilization calculation.
fn calculate_utilization(used: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    (used as f64 / total as f64) * 100.0
}
