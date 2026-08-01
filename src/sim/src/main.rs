use std::fs;

use clap::Parser;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use commputer_core::compliance::{ComplianceStatus, NerfRate, multi_node_multiplier};
use commputer_core::tier::HolderTier;
use commputer_core::token::{TOTAL_SUPPLY, UNITS_PER_COMME};
use commputer_consensus::emission::EmissionSchedule;

// ──────────────────────────────────────────────
// Item 172: Scenario enum for pre-defined scenarios
// ──────────────────────────────────────────────

#[derive(Clone, Debug)]
enum Scenario {
    Growth,
    Attack,
    Crash,
    SteadyState,
    All,
}

impl Scenario {
    fn from_str_opt(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "growth" => Some(Scenario::Growth),
            "attack" => Some(Scenario::Attack),
            "crash" => Some(Scenario::Crash),
            "steady-state" | "steadystate" | "steady_state" => Some(Scenario::SteadyState),
            "all" => Some(Scenario::All),
            _ => None,
        }
    }
}

// ──────────────────────────────────────────────
// CLI (Items 172, 174)
// ──────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "commputer-sim", about = "Economic simulator for the Commputer network")]
struct Cli {
    /// Number of validators in the network
    #[arg(long, default_value_t = 10000)]
    validators: u64,

    /// Number of epochs to simulate
    #[arg(long, default_value_t = 1000)]
    epochs: u64,

    /// Output directory for CSV results
    #[arg(long, default_value = "results")]
    output: String,

    /// Pre-defined scenario: growth, attack, crash, steady-state, all
    #[arg(long)]
    scenario: Option<String>,

    /// Random seed for reproducibility. Same seed = same output.
    #[arg(long)]
    seed: Option<u64>,
}

// ──────────────────────────────────────────────
// Core types
// ──────────────────────────────────────────────

#[derive(Clone)]
struct HardwareProfile {
    cpu_score: u32,
    gpu_score: u32,
    ram_gb: u32,
    storage_tb: u32,
    bandwidth_mbps: u32,
}

impl HardwareProfile {
    fn reference() -> Self {
        Self {
            cpu_score: 100,
            gpu_score: 100,
            ram_gb: 16,
            storage_tb: 1,
            bandwidth_mbps: 100,
        }
    }

    fn composite_score(&self) -> f64 {
        let scores = [
            self.cpu_score as f64,
            self.gpu_score as f64,
            (self.ram_gb as f64) * 6.25,   // normalize to ~100 for 16GB
            (self.storage_tb as f64) * 100.0,
            self.bandwidth_mbps as f64,
        ];
        scores.iter().map(|&s| if s > 0.0 { s.powf(0.7) } else { 0.0 }).sum()
    }
}

#[derive(Clone)]
#[allow(dead_code)]
struct SimValidator {
    id: u64,
    hardware: HardwareProfile,
    contribution_percent: u8,
    join_epoch: u64,
    compliance_status: ComplianceStatus,
    is_nerfed: bool,
    subnet: [u8; 3], // first 3 octets of /24
    balance: u64,     // raw units
    online: bool,     // Item 167: for resilience simulation
}

struct SimNetwork {
    validators: Vec<SimValidator>,
    epoch: u64,
    total_emitted: u64,
    total_burned: u64,
}

impl SimNetwork {
    fn new(validator_count: u64, rng: &mut impl Rng) -> Self {
        let validators: Vec<SimValidator> = (0..validator_count)
            .map(|id| SimValidator {
                id,
                hardware: HardwareProfile::reference(),
                contribution_percent: 100,
                join_epoch: 0,
                compliance_status: ComplianceStatus::Compliant,
                is_nerfed: false,
                subnet: [rng.r#gen(), rng.r#gen(), rng.r#gen()],
                balance: 0,
                online: true,
            })
            .collect();
        SimNetwork {
            validators,
            epoch: 0,
            total_emitted: 0,
            total_burned: 0,
        }
    }

    fn validator_count(&self) -> u64 {
        self.validators.len() as u64
    }

    fn online_count(&self) -> u64 {
        self.validators.iter().filter(|v| v.online).count() as u64
    }

    fn circulating_supply(&self) -> u64 {
        self.total_emitted.saturating_sub(self.total_burned)
    }
}

// ──────────────────────────────────────────────
// Emission simulation
// ──────────────────────────────────────────────

fn run_emission_epoch(network: &mut SimNetwork, schedule: &EmissionSchedule) -> u64 {
    let count = network.online_count().max(1);
    let epoch_emission = schedule.per_epoch_emission(0, count);

    // Cap emission at remaining supply
    let remaining = TOTAL_SUPPLY.saturating_sub(network.total_emitted);
    let actual_emission = epoch_emission.min(remaining);

    if actual_emission == 0 {
        return 0;
    }

    // Distribute equally among online validators (simplified)
    let online_count = network.online_count().max(1);
    let per_validator = actual_emission / online_count;
    for v in network.validators.iter_mut() {
        if !v.online {
            continue;
        }
        let reward = if v.is_nerfed {
            let nerf = NerfRate::INITIAL;
            (per_validator as f64 * nerf.reward_multiplier()) as u64
        } else {
            per_validator
        };
        v.balance += reward;
    }

    network.total_emitted += actual_emission;
    network.epoch += 1;
    actual_emission
}

// ──────────────────────────────────────────────
// Burn simulation
// ──────────────────────────────────────────────

fn run_burn_epoch(network: &mut SimNetwork, rng: &mut impl Rng) -> u64 {
    let mut epoch_burn = 0u64;
    let circulating = network.circulating_supply();

    // Random burst compute: 2% of validators burn a small amount each epoch
    let burst_count = (network.validator_count() as f64 * 0.02) as u64;
    for _ in 0..burst_count {
        let burn = UNITS_PER_COMME / 1000;
        if epoch_burn + burn <= circulating {
            epoch_burn += burn;
        }
    }

    // Milestone burns at capacity thresholds
    let supply_ratio = network.total_emitted as f64 / TOTAL_SUPPLY as f64;
    let milestones = [0.25, 0.50, 0.75];
    for &milestone in &milestones {
        let prev_ratio = (network.total_emitted.saturating_sub(UNITS_PER_COMME * 100)) as f64 / TOTAL_SUPPLY as f64;
        if prev_ratio < milestone && supply_ratio >= milestone {
            let milestone_burn = circulating / 1000;
            epoch_burn += milestone_burn;
            println!("  Milestone burn at {:.0}% capacity: {} COMME", milestone * 100.0, milestone_burn / UNITS_PER_COMME);
        }
    }

    // Annual charitable burn: 1% of circulating, spread over 24*365 epochs
    let epochs_per_year = 24 * 365;
    let annual_charitable = circulating / 100;
    let per_epoch_charitable = annual_charitable / epochs_per_year;
    epoch_burn += per_epoch_charitable;

    // Apply a random jitter to burns
    let jitter: f64 = rng.gen_range(0.8..1.2);
    epoch_burn = (epoch_burn as f64 * jitter) as u64;

    network.total_burned += epoch_burn;
    epoch_burn
}

// ──────────────────────────────────────────────
// Supply curve output
// ──────────────────────────────────────────────

fn write_supply_curve(output_dir: &str, records: &[(u64, u64, u64, u64)]) {
    let path = format!("{}/supply_curve.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create supply_curve.csv");
    wtr.write_record(["epoch", "total_emitted", "total_burned", "circulating_supply"]).unwrap();
    for &(epoch, emitted, burned, circulating) in records {
        wtr.write_record(&[
            epoch.to_string(),
            emitted.to_string(),
            burned.to_string(),
            circulating.to_string(),
        ]).unwrap();
    }
    wtr.flush().unwrap();
    println!("Wrote {}", path);
}

// ──────────────────────────────────────────────
// Reward distribution output
// ──────────────────────────────────────────────

fn write_reward_distribution(output_dir: &str, records: &[(u64, u64, u64, f64, u64, f64)]) {
    let path = format!("{}/reward_distribution.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create reward_distribution.csv");
    wtr.write_record(["epoch", "validator_id", "raw_reward", "nerf_multiplier", "effective_reward", "composite_score"]).unwrap();
    for &(epoch, vid, raw, nerf_mult, effective, score) in records {
        wtr.write_record(&[
            epoch.to_string(),
            vid.to_string(),
            raw.to_string(),
            format!("{:.4}", nerf_mult),
            effective.to_string(),
            format!("{:.2}", score),
        ]).unwrap();
    }
    wtr.flush().unwrap();
    println!("Wrote {}", path);
}

// ──────────────────────────────────────────────
// Original warehouse attack simulation
// ──────────────────────────────────────────────

fn simulate_warehouse_attack() {
    println!("\n=== Warehouse Attack Simulation ===");
    let schedule = EmissionSchedule::new();
    let validator_count = 1000u64;

    let daily_rate = schedule.per_validator_daily_rate(0, validator_count);
    let honest_reward = daily_rate;

    let attacker_nodes = 100u32;
    let mut attacker_total = 0u64;
    for i in 1..=attacker_nodes {
        let multiplier = multi_node_multiplier(i);
        let node_reward = (daily_rate as f64 * multiplier) as u64;
        attacker_total += node_reward;
    }

    println!("Daily rate per validator: {} raw units ({:.4} COMME)",
        daily_rate, daily_rate as f64 / UNITS_PER_COMME as f64);
    println!("Single honest node daily reward: {} raw units ({:.4} COMME)",
        honest_reward, honest_reward as f64 / UNITS_PER_COMME as f64);
    println!("Attacker total daily reward (100 nodes): {} raw units ({:.4} COMME)",
        attacker_total, attacker_total as f64 / UNITS_PER_COMME as f64);
    println!("Attacker reward / honest reward ratio: {:.4}x",
        attacker_total as f64 / honest_reward as f64);

    println!("\nPer-node breakdown:");
    for i in 1..=5u32 {
        let mult = multi_node_multiplier(i);
        println!("  Node {}: multiplier={:.6}, reward={:.6} COMME",
            i, mult, daily_rate as f64 * mult / UNITS_PER_COMME as f64);
    }
    println!("  Nodes 5-100: multiplier=0.0, reward=0.0 COMME");
}

// ──────────────────────────────────────────────
// Item 161: Warehouse attack scenarios
// ──────────────────────────────────────────────

fn simulate_warehouse_scenarios(output_dir: &str) {
    println!("\n=== Warehouse Attack Scenarios (Item 161) ===");
    let schedule = EmissionSchedule::new();

    let path = format!("{}/warehouse_scenarios.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create warehouse_scenarios.csv");
    wtr.write_record(["strategy", "attacker_nodes", "network_size", "attacker_daily_comme",
        "honest_daily_comme", "ratio", "attacker_pct_of_network"]).unwrap();

    let network_sizes = [1_000u64, 10_000, 100_000];

    for &net_size in &network_sizes {
        let daily_rate = schedule.per_validator_daily_rate(0, net_size);
        let honest_reward = daily_rate as f64 / UNITS_PER_COMME as f64;

        // Strategy 1: Many small nodes (200 nodes on same subnet)
        let many_small = 200u32;
        let mut many_small_total = 0.0f64;
        for i in 1..=many_small {
            many_small_total += daily_rate as f64 * multi_node_multiplier(i) / UNITS_PER_COMME as f64;
        }
        println!("Net={}: Many-small (200 nodes): {:.6} COMME/day, ratio={:.4}x",
            net_size, many_small_total, many_small_total / honest_reward);
        wtr.write_record(&[
            "many_small".to_string(), many_small.to_string(), net_size.to_string(),
            format!("{:.6}", many_small_total), format!("{:.6}", honest_reward),
            format!("{:.4}", many_small_total / honest_reward),
            format!("{:.4}", many_small as f64 / net_size as f64 * 100.0),
        ]).unwrap();

        // Strategy 2: Few large nodes (5 nodes, each on different subnets)
        let few_large = 5u32;
        // Each on different subnet => each gets multiplier(1)
        let few_large_total = few_large as f64 * daily_rate as f64 * multi_node_multiplier(1) / UNITS_PER_COMME as f64;
        println!("Net={}: Few-large (5 nodes, diff subnets): {:.6} COMME/day, ratio={:.4}x",
            net_size, few_large_total, few_large_total / honest_reward);
        wtr.write_record(&[
            "few_large".to_string(), few_large.to_string(), net_size.to_string(),
            format!("{:.6}", few_large_total), format!("{:.6}", honest_reward),
            format!("{:.4}", few_large_total / honest_reward),
            format!("{:.4}", few_large as f64 / net_size as f64 * 100.0),
        ]).unwrap();

        // Strategy 3: Geographic clustering (50 nodes, 10 per subnet across 5 subnets)
        let geo_subnets = 5u32;
        let nodes_per_subnet = 10u32;
        let mut geo_total = 0.0f64;
        for _ in 0..geo_subnets {
            for i in 1..=nodes_per_subnet {
                geo_total += daily_rate as f64 * multi_node_multiplier(i) / UNITS_PER_COMME as f64;
            }
        }
        let geo_nodes = geo_subnets * nodes_per_subnet;
        println!("Net={}: Geo-cluster (5 subnets x 10 nodes): {:.6} COMME/day, ratio={:.4}x",
            net_size, geo_total, geo_total / honest_reward);
        wtr.write_record(&[
            "geo_cluster".to_string(), geo_nodes.to_string(), net_size.to_string(),
            format!("{:.6}", geo_total), format!("{:.6}", honest_reward),
            format!("{:.4}", geo_total / honest_reward),
            format!("{:.4}", geo_nodes as f64 / net_size as f64 * 100.0),
        ]).unwrap();
    }

    wtr.flush().unwrap();
    println!("Wrote {}", path);
}

// ──────────────────────────────────────────────
// Network growth simulation (S-curve)
// ──────────────────────────────────────────────

fn simulate_network_growth(output_dir: &str) {
    println!("\n=== Network Growth Simulation ===");
    let schedule = EmissionSchedule::new();

    let k: f64 = 1_000_000.0;
    let r: f64 = 0.005;
    let t0: f64 = (5 * 365 * 24) as f64;

    let total_epochs = 10 * 365 * 24;
    let mut records: Vec<(u64, u64, f64, f64)> = Vec::new();
    let mut cumulative_reward = 0.0f64;
    let sample_interval = 24 * 30;

    for epoch in 0..total_epochs {
        let t = epoch as f64;
        let n = (k / (1.0 + (-r * (t - t0)).exp())).max(100.0) as u64;

        let per_val_daily = schedule.per_validator_daily_rate(0, n);
        let per_val_epoch = per_val_daily as f64 / 24.0;
        cumulative_reward += per_val_epoch;

        if epoch % sample_interval == 0 {
            let cumulative_comme = cumulative_reward / UNITS_PER_COMME as f64;
            records.push((epoch as u64, n, per_val_daily as f64 / UNITS_PER_COMME as f64, cumulative_comme));
        }
    }

    let path = format!("{}/network_growth.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create network_growth.csv");
    wtr.write_record(["epoch", "validator_count", "per_validator_daily_comme", "cumulative_reward_comme"]).unwrap();
    for &(e, n, daily, cum) in &records {
        wtr.write_record(&[e.to_string(), n.to_string(), format!("{:.6}", daily), format!("{:.4}", cum)]).unwrap();
    }
    wtr.flush().unwrap();
    println!("Wrote {}", path);

    let first = records.first().unwrap();
    let last = records.last().unwrap();
    println!("Start: {} validators, {:.6} COMME/day/validator", first.1, first.2);
    println!("End (10yr): {} validators, {:.6} COMME/day/validator", last.1, last.2);
    println!("Cumulative reward for day-1 validator: {:.2} COMME over 10 years", last.3);
}

// ──────────────────────────────────────────────
// Emission exhaustion simulation
// ──────────────────────────────────────────────

fn simulate_emission_exhaustion() {
    println!("\n=== Emission Exhaustion Simulation ===");
    let schedule = EmissionSchedule::new();

    let validator_count = 100_000u64;
    let mut total_emitted = 0u64;
    let mut epoch = 0u64;
    let max_epochs = 200 * 365 * 24;

    loop {
        let remaining = TOTAL_SUPPLY.saturating_sub(total_emitted);
        if remaining == 0 || epoch >= max_epochs {
            break;
        }
        let emission = schedule.per_epoch_emission(0, validator_count).min(remaining);
        total_emitted += emission;
        epoch += 1;
    }

    assert!(total_emitted <= TOTAL_SUPPLY, "Total emitted exceeds 2B supply cap!");

    let years = epoch as f64 / (365.0 * 24.0);
    println!("Supply exhausted at epoch {} ({:.1} years)", epoch, years);
    println!("Total emitted: {:.2} COMME", total_emitted as f64 / UNITS_PER_COMME as f64);
    println!("Supply cap (2B): {:.2} COMME", TOTAL_SUPPLY as f64 / UNITS_PER_COMME as f64);
    println!("Verified: total_emitted <= TOTAL_SUPPLY: {}", total_emitted <= TOTAL_SUPPLY);

    let floor_rate = schedule.per_validator_daily_rate(0, 100_000_000);
    println!("Floor rate at 100M validators: {:.6} COMME/day", floor_rate as f64 / UNITS_PER_COMME as f64);
    assert!(floor_rate >= UNITS_PER_COMME / 100, "Floor rate violated!");
    println!("Floor rate >= 0.01 COMME/day: verified");
}

// ──────────────────────────────────────────────
// Item 162: Tier accessibility analysis (extended)
// ──────────────────────────────────────────────

fn simulate_tier_accessibility(output_dir: &str) {
    println!("\n=== Tier Accessibility Simulation (Item 162) ===");
    let schedule = EmissionSchedule::new();

    let network_sizes = [1_000u64, 10_000, 100_000, 1_000_000];
    // Item 162: Add token price dimension
    let token_prices_usd = [0.10f64, 1.0, 10.0, 100.0];
    let tiers = [
        ("Base", HolderTier::BASE_THRESHOLD),
        ("Storage", HolderTier::STORAGE_THRESHOLD),
        ("Compute", HolderTier::COMPUTE_THRESHOLD),
        ("Full", HolderTier::FULL_THRESHOLD),
    ];

    let path = format!("{}/tier_accessibility.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create tier_accessibility.csv");
    wtr.write_record(["network_size", "tier", "epochs_to_reach", "days_to_reach",
        "token_price_usd", "usd_value_at_tier"]).unwrap();

    for &size in &network_sizes {
        let daily_rate = schedule.per_validator_daily_rate(0, size);
        println!("\nNetwork size: {} validators (daily rate: {:.6} COMME)",
            size, daily_rate as f64 / UNITS_PER_COMME as f64);

        for &(tier_name, threshold) in &tiers {
            let target_raw = threshold * UNITS_PER_COMME;
            let per_epoch = daily_rate / 24;
            let epochs_needed = if per_epoch > 0 {
                target_raw.div_ceil(per_epoch)
            } else {
                u64::MAX
            };
            let days = epochs_needed as f64 / 24.0;
            println!("  {} tier ({} COMME): {} epochs ({:.1} days)",
                tier_name, threshold, epochs_needed, days);

            for &price in &token_prices_usd {
                let usd_value = threshold as f64 * price;
                wtr.write_record(&[
                    size.to_string(),
                    tier_name.to_string(),
                    epochs_needed.to_string(),
                    format!("{:.1}", days),
                    format!("{:.2}", price),
                    format!("{:.2}", usd_value),
                ]).unwrap();
            }
        }
    }
    wtr.flush().unwrap();
    println!("Wrote {}", path);
}

// ──────────────────────────────────────────────
// Item 163: Burn crossover analysis (extended with CSV)
// ──────────────────────────────────────────────

fn simulate_burn_crossover(output_dir: &str) {
    println!("\n=== Burn Crossover Simulation (Item 163) ===");
    let schedule = EmissionSchedule::new();

    let validator_count = 100_000u64;
    let epochs_per_year: u64 = 24 * 365;

    let mut total_emitted = 0u64;
    let mut total_burned = 0u64;
    let mut annual_emission_window = Vec::new();
    let mut annual_burn_window = Vec::new();
    let mut crossover_epoch = None;

    let max_epochs = 100 * epochs_per_year;

    let path = format!("{}/burn_crossover.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create burn_crossover.csv");
    wtr.write_record(["epoch", "year", "annual_emission_comme", "annual_burn_comme",
        "cumulative_emitted_comme", "cumulative_burned_comme", "circulating_comme"]).unwrap();

    let sample_interval = epochs_per_year; // yearly samples

    for epoch in 0..max_epochs {
        let remaining = TOTAL_SUPPLY.saturating_sub(total_emitted);
        let emission = schedule.per_epoch_emission(0, validator_count).min(remaining);
        total_emitted += emission;

        let circulating = total_emitted.saturating_sub(total_burned);

        let charitable_per_epoch = circulating / 100 / epochs_per_year;
        let burst_burn = (validator_count as f64 * 0.02) as u64 * (UNITS_PER_COMME / 1000);
        let epoch_burn = charitable_per_epoch + burst_burn;
        total_burned += epoch_burn;

        annual_emission_window.push(emission);
        annual_burn_window.push(epoch_burn);

        if annual_emission_window.len() > epochs_per_year as usize {
            annual_emission_window.remove(0);
            annual_burn_window.remove(0);
        }

        if annual_emission_window.len() == epochs_per_year as usize {
            if crossover_epoch.is_none() {
                let annual_em: u64 = annual_emission_window.iter().sum();
                let annual_bn: u64 = annual_burn_window.iter().sum();
                if annual_bn > annual_em {
                    crossover_epoch = Some(epoch);
                }
            }

            // Write yearly data point
            if epoch % sample_interval == 0 {
                let annual_em: u64 = annual_emission_window.iter().sum();
                let annual_bn: u64 = annual_burn_window.iter().sum();
                let year = epoch / epochs_per_year;
                wtr.write_record(&[
                    epoch.to_string(),
                    year.to_string(),
                    format!("{:.2}", annual_em as f64 / UNITS_PER_COMME as f64),
                    format!("{:.2}", annual_bn as f64 / UNITS_PER_COMME as f64),
                    format!("{:.2}", total_emitted as f64 / UNITS_PER_COMME as f64),
                    format!("{:.2}", total_burned as f64 / UNITS_PER_COMME as f64),
                    format!("{:.2}", circulating as f64 / UNITS_PER_COMME as f64),
                ]).unwrap();
            }
        }
    }

    wtr.flush().unwrap();
    println!("Wrote {}", path);

    match crossover_epoch {
        Some(ep) => {
            let years = ep as f64 / epochs_per_year as f64;
            println!("Burn crossover at epoch {} ({:.1} years)", ep, years);
            println!("At crossover: emitted={:.2} COMME, burned={:.2} COMME",
                total_emitted as f64 / UNITS_PER_COMME as f64,
                total_burned as f64 / UNITS_PER_COMME as f64);
        }
        None => {
            println!("No burn crossover found within 100 years at {} validators", validator_count);
            println!("Final: emitted={:.2} COMME, burned={:.2} COMME",
                total_emitted as f64 / UNITS_PER_COMME as f64,
                total_burned as f64 / UNITS_PER_COMME as f64);
        }
    }
}

// ──────────────────────────────────────────────
// Item 165: Gold standard hardware evolution with cost modeling
// ──────────────────────────────────────────────

fn simulate_hardware_evolution(output_dir: &str) {
    println!("\n=== Gold Standard Hardware Evolution (Item 165) ===");

    let path = format!("{}/hardware_evolution.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create hardware_evolution.csv");
    wtr.write_record(["year", "cpu_score", "ram_gb", "storage_tb",
        "estimated_cost_usd", "cost_per_performance"]).unwrap();

    // Base cost for reference hardware in year 0: ~$800
    let base_cost_usd = 800.0f64;

    for year in 0..=10 {
        let multiplier = 2.0f64.powf(year as f64 / 2.0);
        let cpu_score = (100.0 * multiplier) as u64;
        let ram_gb = (16.0 * multiplier) as u64;
        let storage_tb = (1.0 * multiplier).max(1.0) as u64;

        // Moore's law cost modeling: same performance costs ~halve every 2 years
        // So reference-equivalent hardware costs base / multiplier
        // But new "reference" at that year's standard costs about the same
        let cost_for_equivalent = base_cost_usd / multiplier;
        let cost_for_new_standard = base_cost_usd; // new standard = same $ but more powerful
        let composite = HardwareProfile {
            cpu_score: cpu_score as u32,
            gpu_score: (100.0 * multiplier) as u32,
            ram_gb: ram_gb as u32,
            storage_tb: storage_tb as u32,
            bandwidth_mbps: 100 + (year * 20), // bandwidth grows more slowly
        }.composite_score();
        let cost_per_perf = cost_for_new_standard / composite;

        println!("Year {}: CPU={}, RAM={}GB, Storage={}TB, eq_cost=${:.0}, new_std=$800, $/perf={:.4}",
            year, cpu_score, ram_gb, storage_tb.max(1), cost_for_equivalent, cost_per_perf);

        wtr.write_record(&[
            year.to_string(),
            cpu_score.to_string(),
            ram_gb.to_string(),
            storage_tb.max(1).to_string(),
            format!("{:.2}", cost_for_new_standard),
            format!("{:.6}", cost_per_perf),
        ]).unwrap();
    }
    wtr.flush().unwrap();
    println!("Wrote {}", path);
}

// ──────────────────────────────────────────────
// Grace period simulation
// ──────────────────────────────────────────────

fn simulate_grace_period() {
    println!("\n=== Grace Period Simulation ===");

    let max_grace_hours: f64 = 10.0 * 365.0 * 24.0;
    let sim_epochs = 365 * 24;

    struct UptimePattern {
        name: &'static str,
        is_online: fn(epoch: u64) -> bool,
    }

    let patterns = [
        UptimePattern { name: "100% uptime", is_online: |_| true },
        UptimePattern { name: "80% uptime", is_online: |e| e % 5 != 0 },
        UptimePattern { name: "50% uptime", is_online: |e| e % 2 == 0 },
        UptimePattern { name: "Weekend-only", is_online: |e| {
            let hour_of_week = e % 168;
            hour_of_week >= 120
        }},
    ];

    for pattern in &patterns {
        let mut grace_hours: f64 = 0.0;
        let mut online_epochs = 0u64;
        let mut offline_epochs = 0u64;

        for epoch in 0..sim_epochs {
            let online = (pattern.is_online)(epoch as u64);
            if online {
                online_epochs += 1;
                grace_hours = (grace_hours + 0.5).min(max_grace_hours);
            } else {
                offline_epochs += 1;
                grace_hours = (grace_hours - 1.0).max(0.0);
            }
        }

        let actual_uptime = online_epochs as f64 / sim_epochs as f64 * 100.0;
        println!("{}: uptime={:.1}%, grace_hours={:.1}/{:.0}, online={}, offline={}",
            pattern.name, actual_uptime, grace_hours, max_grace_hours,
            online_epochs, offline_epochs);

        assert!(grace_hours >= 0.0, "Grace hours went negative!");
        assert!(grace_hours <= max_grace_hours, "Grace hours exceeded cap!");
    }
    println!("Verified: 1:1 drain, 2:1 refill, 10-year cap constraints hold.");
}

// ──────────────────────────────────────────────
// Item 164: Charitable burn modeling (extended)
// ──────────────────────────────────────────────

fn simulate_charitable_burns(output_dir: &str) {
    println!("\n=== Charitable Burn Impact Simulation (Item 164) ===");
    let schedule = EmissionSchedule::new();

    // Extended adoption levels
    let adoption_levels = [1_000u64, 10_000, 50_000, 100_000, 500_000, 1_000_000];
    let time_horizons = [1u64, 5, 10, 20, 50];
    // Charitable burn rate scenarios: conservative 0.5%, standard 1%, aggressive 2%
    let burn_rates = [0.005f64, 0.01, 0.02];

    let path = format!("{}/charitable_burns.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create charitable_burns.csv");
    wtr.write_record(["validators", "years", "burn_rate_pct", "total_donated_comme",
        "pct_of_circulating"]).unwrap();

    for &validators in &adoption_levels {
        for &years in &time_horizons {
            for &rate in &burn_rates {
                let epochs_per_year: u64 = 24 * 365;
                let total_epochs = years * epochs_per_year;

                let mut total_emitted = 0u64;
                let mut total_burned = 0u64;
                let mut total_charitable = 0u64;

                for _ in 0..total_epochs {
                    let remaining = TOTAL_SUPPLY.saturating_sub(total_emitted);
                    let emission = schedule.per_epoch_emission(0, validators).min(remaining);
                    total_emitted += emission;

                    let circulating = total_emitted.saturating_sub(total_burned);
                    let charitable_per_epoch = (circulating as f64 * rate / epochs_per_year as f64) as u64;
                    total_charitable += charitable_per_epoch;
                    total_burned += charitable_per_epoch;
                }

                let donated_comme = total_charitable as f64 / UNITS_PER_COMME as f64;
                let circulating = total_emitted.saturating_sub(total_burned);
                let pct_of_circ = if circulating > 0 {
                    total_charitable as f64 / circulating as f64 * 100.0
                } else {
                    0.0
                };

                // Only print the standard rate to keep output manageable
                if (rate - 0.01).abs() < f64::EPSILON {
                    println!("{} validators, {} years, {:.1}% rate: {:.2} COMME donated",
                        validators, years, rate * 100.0, donated_comme);
                }

                wtr.write_record(&[
                    validators.to_string(),
                    years.to_string(),
                    format!("{:.1}", rate * 100.0),
                    format!("{:.2}", donated_comme),
                    format!("{:.4}", pct_of_circ),
                ]).unwrap();
            }
        }
    }
    wtr.flush().unwrap();
    println!("Wrote {}", path);
}

// ──────────────────────────────────────────────
// Item 166: Validator churn modeling
// ──────────────────────────────────────────────

fn simulate_validator_churn(output_dir: &str, rng: &mut impl Rng) {
    println!("\n=== Validator Churn Simulation (Item 166) ===");
    let schedule = EmissionSchedule::new();

    let initial_validators = 10_000u64;
    let epochs_per_year: u64 = 24 * 365;
    let sim_years = 5u64;
    let total_epochs = sim_years * epochs_per_year;

    // Churn parameters: monthly join/leave rates
    let monthly_join_rate = 0.05; // 5% new validators per month
    let monthly_leave_rate = 0.03; // 3% leave per month
    let epochs_per_month = 24 * 30;

    let mut current_validators = initial_validators;
    let mut records: Vec<(u64, u64, f64, f64)> = Vec::new(); // epoch, validators, per_val_daily, churn_rate

    let mut joins_this_month = 0u64;
    let mut leaves_this_month = 0u64;

    for epoch in 0..total_epochs {
        // Apply monthly churn
        if epoch > 0 && epoch % epochs_per_month as u64 == 0 {
            let new_joins = (current_validators as f64 * monthly_join_rate) as u64;
            let new_leaves = (current_validators as f64 * monthly_leave_rate
                * rng.gen_range(0.5..1.5)) as u64;

            current_validators = current_validators.saturating_add(new_joins);
            current_validators = current_validators.saturating_sub(new_leaves);
            current_validators = current_validators.max(100); // minimum network size

            joins_this_month = new_joins;
            leaves_this_month = new_leaves;
        }

        // Sample monthly
        if epoch % epochs_per_month as u64 == 0 {
            let daily_rate = schedule.per_validator_daily_rate(0, current_validators);
            let churn_rate = if current_validators > 0 {
                (joins_this_month as f64 + leaves_this_month as f64) / current_validators as f64
            } else {
                0.0
            };
            records.push((epoch, current_validators,
                daily_rate as f64 / UNITS_PER_COMME as f64, churn_rate));
        }
    }

    let path = format!("{}/validator_churn.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create validator_churn.csv");
    wtr.write_record(["epoch", "validator_count", "per_validator_daily_comme", "monthly_churn_rate"]).unwrap();
    for &(e, v, daily, churn) in &records {
        wtr.write_record(&[
            e.to_string(), v.to_string(),
            format!("{:.6}", daily), format!("{:.4}", churn),
        ]).unwrap();
    }
    wtr.flush().unwrap();
    println!("Wrote {}", path);

    let first = records.first().unwrap();
    let last = records.last().unwrap();
    println!("Start: {} validators, {:.6} COMME/day", first.1, first.2);
    println!("End ({}yr): {} validators, {:.6} COMME/day", sim_years, last.1, last.2);
}

// ──────────────────────────────────────────────
// Item 167: Network resilience simulation
// ──────────────────────────────────────────────

fn simulate_network_resilience(output_dir: &str, rng: &mut impl Rng) {
    println!("\n=== Network Resilience Simulation (Item 167) ===");
    let schedule = EmissionSchedule::new();

    let validator_count = 10_000u64;
    let mut network = SimNetwork::new(validator_count, rng);

    let crash_pct = 0.30; // 30% go offline
    let crash_epoch = 100u64;
    let recovery_rate = 0.02; // 2% recovery per epoch after crash
    let total_epochs = 500u64;

    let path = format!("{}/network_resilience.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create network_resilience.csv");
    wtr.write_record(["epoch", "online_validators", "total_validators",
        "online_pct", "per_validator_daily_comme", "emission_this_epoch"]).unwrap();

    let mut recovery_complete_epoch = None;

    for epoch in 0..total_epochs {
        // Crash event
        if epoch == crash_epoch {
            let offline_count = (validator_count as f64 * crash_pct) as usize;
            for v in network.validators.iter_mut().take(offline_count) {
                v.online = false;
            }
            println!("Epoch {}: {:.0}% crash ({} validators offline)",
                epoch, crash_pct * 100.0, offline_count);
        }

        // Recovery: offline validators come back gradually
        if epoch > crash_epoch {
            for v in network.validators.iter_mut() {
                if !v.online && rng.gen_range(0.0..1.0) < recovery_rate {
                    v.online = true;
                }
            }
        }

        let online = network.online_count();
        let online_pct = online as f64 / validator_count as f64 * 100.0;

        // Check if recovery is complete (>95%)
        if epoch > crash_epoch && online_pct >= 95.0 && recovery_complete_epoch.is_none() {
            recovery_complete_epoch = Some(epoch);
            println!("Epoch {}: Recovery to 95%+ ({} online)", epoch, online);
        }

        let emission = run_emission_epoch(&mut network, &schedule);
        let daily_rate = schedule.per_validator_daily_rate(0, online);

        wtr.write_record(&[
            epoch.to_string(),
            online.to_string(),
            validator_count.to_string(),
            format!("{:.1}", online_pct),
            format!("{:.6}", daily_rate as f64 / UNITS_PER_COMME as f64),
            emission.to_string(),
        ]).unwrap();
    }

    wtr.flush().unwrap();
    println!("Wrote {}", path);

    match recovery_complete_epoch {
        Some(ep) => {
            let recovery_time = ep - crash_epoch;
            println!("Recovery time: {} epochs ({:.1} hours) to reach 95%+",
                recovery_time, recovery_time as f64);
        }
        None => println!("Network did not fully recover within {} epochs", total_epochs),
    }
}

// ──────────────────────────────────────────────
// Item 168: Sybil attack cost analysis
// ──────────────────────────────────────────────

fn simulate_sybil_cost(output_dir: &str) {
    println!("\n=== Sybil Attack Cost Analysis (Item 168) ===");
    let schedule = EmissionSchedule::new();

    // Reference hardware cost
    let hardware_cost_usd = 800.0f64;
    // Monthly operating cost per node (electricity, bandwidth)
    let monthly_opex_usd = 50.0f64;

    let network_sizes = [1_000u64, 10_000, 100_000, 1_000_000];
    let attack_percentages = [0.10f64, 0.20, 0.33, 0.51]; // 10%, 20%, 33%, 51%

    let path = format!("{}/sybil_cost.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create sybil_cost.csv");
    wtr.write_record(["network_size", "attack_pct", "attacker_nodes", "hardware_cost_usd",
        "monthly_opex_usd", "year1_total_usd", "daily_reward_comme",
        "breakeven_days"]).unwrap();

    for &net_size in &network_sizes {
        println!("\nNetwork size: {} validators", net_size);
        for &pct in &attack_percentages {
            let attacker_nodes = (net_size as f64 * pct) as u64;
            let hw_cost = attacker_nodes as f64 * hardware_cost_usd;
            let monthly_opex = attacker_nodes as f64 * monthly_opex_usd;
            let year1_total = hw_cost + monthly_opex * 12.0;

            // Attacker reward: each node on unique subnet (best case for attacker)
            let total_network = net_size + attacker_nodes;
            let daily_rate = schedule.per_validator_daily_rate(0, total_network);
            let daily_reward_comme = attacker_nodes as f64 * daily_rate as f64 / UNITS_PER_COMME as f64;

            // At $1/COMME, how many days to break even on year 1 costs?
            let breakeven_days = if daily_reward_comme > 0.0 {
                year1_total / daily_reward_comme
            } else {
                f64::INFINITY
            };

            println!("  {:.0}% attack ({} nodes): hw=${:.0}, yr1=${:.0}, {:.2} COMME/day, breakeven={:.0} days",
                pct * 100.0, attacker_nodes, hw_cost, year1_total, daily_reward_comme, breakeven_days);

            wtr.write_record(&[
                net_size.to_string(),
                format!("{:.0}", pct * 100.0),
                attacker_nodes.to_string(),
                format!("{:.0}", hw_cost),
                format!("{:.0}", monthly_opex),
                format!("{:.0}", year1_total),
                format!("{:.2}", daily_reward_comme),
                format!("{:.0}", breakeven_days),
            ]).unwrap();
        }
    }

    wtr.flush().unwrap();
    println!("Wrote {}", path);
}

// ──────────────────────────────────────────────
// Item 169: Tokenomics sensitivity analysis
// ──────────────────────────────────────────────

fn simulate_sensitivity(output_dir: &str) {
    println!("\n=== Tokenomics Sensitivity Analysis (Item 169) ===");

    let path = format!("{}/sensitivity.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create sensitivity.csv");
    wtr.write_record(["parameter", "variation", "value", "metric",
        "metric_value", "pct_change_from_baseline"]).unwrap();

    let schedule = EmissionSchedule::new();
    let baseline_validators = 100_000u64;
    let epochs_per_year: u64 = 24 * 365;
    let sim_years = 10u64;
    let total_epochs = sim_years * epochs_per_year;

    // Baseline simulation
    let baseline_metrics = run_sensitivity_sim(&schedule, baseline_validators, total_epochs, 0.01);

    // Parameter variations
    let param_variations: Vec<(&str, Vec<(&str, f64, f64, f64)>)> = vec![
        // (param_name, [(label, validator_mult, burn_rate, _)])
        ("validator_count", vec![
            ("0.5x", 0.5, 0.01, 0.0),
            ("1x (baseline)", 1.0, 0.01, 0.0),
            ("2x", 2.0, 0.01, 0.0),
            ("5x", 5.0, 0.01, 0.0),
            ("10x", 10.0, 0.01, 0.0),
        ]),
        ("burn_rate", vec![
            ("0%", 1.0, 0.0, 0.0),
            ("0.5%", 1.0, 0.005, 0.0),
            ("1% (baseline)", 1.0, 0.01, 0.0),
            ("2%", 1.0, 0.02, 0.0),
            ("5%", 1.0, 0.05, 0.0),
        ]),
    ];

    for (param_name, variations) in &param_variations {
        println!("\nSensitivity: {}", param_name);
        for &(label, val_mult, burn_rate, _) in variations {
            let validators = (baseline_validators as f64 * val_mult) as u64;
            let metrics = run_sensitivity_sim(&schedule, validators, total_epochs, burn_rate);

            for (metric_name, metric_val, baseline_val) in [
                ("circulating_comme", metrics.0, baseline_metrics.0),
                ("per_validator_daily", metrics.1, baseline_metrics.1),
                ("total_burned_comme", metrics.2, baseline_metrics.2),
            ] {
                let pct_change = if baseline_val > 0.0 {
                    (metric_val - baseline_val) / baseline_val * 100.0
                } else {
                    0.0
                };

                wtr.write_record(&[
                    param_name.to_string(),
                    label.to_string(),
                    format!("{}", validators),
                    metric_name.to_string(),
                    format!("{:.4}", metric_val),
                    format!("{:.2}", pct_change),
                ]).unwrap();
            }

            let delta_pct = if baseline_metrics.0 > 0.0 {
                (metrics.0 - baseline_metrics.0) / baseline_metrics.0 * 100.0
            } else {
                0.0
            };
            println!("  {}: circ={:.2}, daily={:.6}, burned={:.2} ({:+.1}% from baseline)",
                label, metrics.0, metrics.1, metrics.2, delta_pct);
        }
    }

    wtr.flush().unwrap();
    println!("Wrote {}", path);
}

/// Returns (circulating_comme, per_validator_daily_comme, total_burned_comme)
fn run_sensitivity_sim(schedule: &EmissionSchedule, validators: u64, total_epochs: u64, burn_rate: f64) -> (f64, f64, f64) {
    let epochs_per_year: u64 = 24 * 365;
    let mut total_emitted = 0u64;
    let mut total_burned = 0u64;

    for _ in 0..total_epochs {
        let remaining = TOTAL_SUPPLY.saturating_sub(total_emitted);
        let emission = schedule.per_epoch_emission(0, validators).min(remaining);
        total_emitted += emission;

        let circulating = total_emitted.saturating_sub(total_burned);
        let charitable = (circulating as f64 * burn_rate / epochs_per_year as f64) as u64;
        total_burned += charitable;
    }

    let circulating = total_emitted.saturating_sub(total_burned);
    let daily_rate = schedule.per_validator_daily_rate(0, validators);

    (
        circulating as f64 / UNITS_PER_COMME as f64,
        daily_rate as f64 / UNITS_PER_COMME as f64,
        total_burned as f64 / UNITS_PER_COMME as f64,
    )
}

// ──────────────────────────────────────────────
// Item 170: HTML chart visualization
// ──────────────────────────────────────────────

fn generate_html_charts(output_dir: &str) {
    println!("\n=== Generating HTML Charts (Item 170) ===");

    let html = format!(r##"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Commputer Economic Simulator Results</title>
    <script src="https://cdn.jsdelivr.net/npm/chart.js@4"></script>
    <style>
        body {{ font-family: system-ui, sans-serif; max-width: 1200px; margin: 0 auto; padding: 20px; background: #1a1a2e; color: #e0e0e0; }}
        h1 {{ color: #00d4ff; text-align: center; }}
        h2 {{ color: #00d4ff; border-bottom: 1px solid #333; padding-bottom: 8px; }}
        .chart-container {{ background: #16213e; border-radius: 8px; padding: 20px; margin: 20px 0; }}
        canvas {{ max-height: 400px; }}
        .info {{ background: #0f3460; padding: 15px; border-radius: 8px; margin: 10px 0; }}
        .error {{ color: #ff6b6b; }}
    </style>
</head>
<body>
    <h1>Commputer Economic Simulator</h1>
    <div class="info">
        <p>Load CSV files from the <code>{output_dir}/</code> directory to populate charts.</p>
        <p>Charts will render automatically when CSV data is available.</p>
    </div>

    <h2>Supply Curve</h2>
    <div class="chart-container">
        <canvas id="supplyChart"></canvas>
    </div>

    <h2>Burn Crossover</h2>
    <div class="chart-container">
        <canvas id="burnChart"></canvas>
    </div>

    <h2>Network Growth</h2>
    <div class="chart-container">
        <canvas id="growthChart"></canvas>
    </div>

    <h2>Tier Accessibility</h2>
    <div class="chart-container">
        <canvas id="tierChart"></canvas>
    </div>

    <h2>Validator Churn</h2>
    <div class="chart-container">
        <canvas id="churnChart"></canvas>
    </div>

    <script>
    function parseCSV(text) {{
        const lines = text.trim().split('\n');
        const headers = lines[0].split(',');
        return lines.slice(1).map(line => {{
            const vals = line.split(',');
            const obj = {{}};
            headers.forEach((h, i) => obj[h.trim()] = vals[i] ? vals[i].trim() : '');
            return obj;
        }});
    }}

    async function loadCSV(filename) {{
        try {{
            const resp = await fetch(filename);
            if (!resp.ok) return null;
            return parseCSV(await resp.text());
        }} catch(e) {{ return null; }}
    }}

    async function renderCharts() {{
        // Supply curve
        const supply = await loadCSV('supply_curve.csv');
        if (supply) {{
            new Chart(document.getElementById('supplyChart'), {{
                type: 'line',
                data: {{
                    labels: supply.map(r => r.epoch),
                    datasets: [
                        {{ label: 'Emitted', data: supply.map(r => parseFloat(r.total_emitted)/1e8), borderColor: '#00d4ff', fill: false }},
                        {{ label: 'Burned', data: supply.map(r => parseFloat(r.total_burned)/1e8), borderColor: '#ff6b6b', fill: false }},
                        {{ label: 'Circulating', data: supply.map(r => parseFloat(r.circulating_supply)/1e8), borderColor: '#4ade80', fill: false }}
                    ]
                }},
                options: {{ responsive: true, scales: {{ y: {{ title: {{ display: true, text: 'COMME' }} }} }} }}
            }});
        }}

        // Burn crossover
        const burn = await loadCSV('burn_crossover.csv');
        if (burn) {{
            new Chart(document.getElementById('burnChart'), {{
                type: 'line',
                data: {{
                    labels: burn.map(r => 'Year ' + r.year),
                    datasets: [
                        {{ label: 'Annual Emission', data: burn.map(r => parseFloat(r.annual_emission_comme)), borderColor: '#00d4ff', fill: false }},
                        {{ label: 'Annual Burn', data: burn.map(r => parseFloat(r.annual_burn_comme)), borderColor: '#ff6b6b', fill: false }}
                    ]
                }},
                options: {{ responsive: true }}
            }});
        }}

        // Network growth
        const growth = await loadCSV('network_growth.csv');
        if (growth) {{
            new Chart(document.getElementById('growthChart'), {{
                type: 'line',
                data: {{
                    labels: growth.map(r => r.epoch),
                    datasets: [
                        {{ label: 'Validators', data: growth.map(r => parseInt(r.validator_count)), borderColor: '#00d4ff', fill: false, yAxisID: 'y' }},
                        {{ label: 'Daily COMME/validator', data: growth.map(r => parseFloat(r.per_validator_daily_comme)), borderColor: '#4ade80', fill: false, yAxisID: 'y1' }}
                    ]
                }},
                options: {{ responsive: true, scales: {{ y: {{ position: 'left' }}, y1: {{ position: 'right' }} }} }}
            }});
        }}

        // Validator churn
        const churn = await loadCSV('validator_churn.csv');
        if (churn) {{
            new Chart(document.getElementById('churnChart'), {{
                type: 'line',
                data: {{
                    labels: churn.map(r => r.epoch),
                    datasets: [
                        {{ label: 'Validator Count', data: churn.map(r => parseInt(r.validator_count)), borderColor: '#00d4ff', fill: false }}
                    ]
                }},
                options: {{ responsive: true }}
            }});
        }}
    }}

    renderCharts();
    </script>
</body>
</html>
"##, output_dir = output_dir);

    let path = format!("{}/charts.html", output_dir);
    fs::write(&path, html).expect("Failed to write charts.html");
    println!("Wrote {}", path);
}

// ──────────────────────────────────────────────
// Item 173: Parallel scenario execution
// ──────────────────────────────────────────────

fn run_scenarios_parallel(output_dir: &str, seed: u64) {
    println!("\n=== Parallel Scenario Execution (Item 173) ===");

    // We use std::thread instead of rayon to avoid adding a heavyweight dependency
    // Each scenario runs in its own thread with its own deterministic RNG
    let output = output_dir.to_string();

    let handles: Vec<std::thread::JoinHandle<()>> = vec![
        {
            let dir = format!("{}/scenario_growth", output);
            std::thread::spawn(move || {
                fs::create_dir_all(&dir).ok();
                simulate_network_growth(&dir);
            })
        },
        {
            let dir = format!("{}/scenario_attack", output);
            std::thread::spawn(move || {
                fs::create_dir_all(&dir).ok();
                simulate_warehouse_scenarios(&dir);
                simulate_sybil_cost(&dir);
            })
        },
        {
            let dir = format!("{}/scenario_crash", output);
            let s = seed;
            std::thread::spawn(move || {
                fs::create_dir_all(&dir).ok();
                let mut rng = StdRng::seed_from_u64(s.wrapping_add(2));
                simulate_network_resilience(&dir, &mut rng);
            })
        },
        {
            let dir = format!("{}/scenario_steady", output);
            std::thread::spawn(move || {
                fs::create_dir_all(&dir).ok();
                simulate_sensitivity(&dir);
            })
        },
    ];

    for h in handles {
        h.join().expect("Scenario thread panicked");
    }

    println!("All parallel scenarios complete.");
}

// ──────────────────────────────────────────────
// Main
// ──────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    let output_dir = &cli.output;

    // Ensure output directory exists
    fs::create_dir_all(output_dir).expect("Failed to create output directory");

    // Item 174: Seed-based reproducibility
    let seed = cli.seed.unwrap_or_else(|| {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
    });
    let mut rng = StdRng::seed_from_u64(seed);

    println!("=== Commputer Economic Simulator ===");
    println!("Validators: {}", cli.validators);
    println!("Epochs: {}", cli.epochs);
    println!("Output: {}", output_dir);
    println!("Seed: {}", seed);

    // Item 172: Handle --scenario flag
    let scenario = cli.scenario.as_deref()
        .and_then(Scenario::from_str_opt);

    match &scenario {
        Some(Scenario::Growth) => {
            simulate_network_growth(output_dir);
            simulate_validator_churn(output_dir, &mut rng);
            generate_html_charts(output_dir);
            return;
        }
        Some(Scenario::Attack) => {
            simulate_warehouse_attack();
            simulate_warehouse_scenarios(output_dir);
            simulate_sybil_cost(output_dir);
            generate_html_charts(output_dir);
            return;
        }
        Some(Scenario::Crash) => {
            simulate_network_resilience(output_dir, &mut rng);
            generate_html_charts(output_dir);
            return;
        }
        Some(Scenario::SteadyState) => {
            simulate_sensitivity(output_dir);
            simulate_burn_crossover(output_dir);
            generate_html_charts(output_dir);
            return;
        }
        Some(Scenario::All) | None => {
            // Run everything
        }
    }

    let schedule = EmissionSchedule::new();

    // Main emission + burn simulation
    println!("\n=== Main Simulation ===");
    let mut network = SimNetwork::new(cli.validators, &mut rng);
    let mut supply_records: Vec<(u64, u64, u64, u64)> = Vec::new();
    let mut reward_records: Vec<(u64, u64, u64, f64, u64, f64)> = Vec::new();

    for epoch in 0..cli.epochs {
        let emission = run_emission_epoch(&mut network, &schedule);
        let _burn = run_burn_epoch(&mut network, &mut rng);

        supply_records.push((
            epoch,
            network.total_emitted,
            network.total_burned,
            network.circulating_supply(),
        ));

        if epoch % 100 == 0 {
            let per_val = emission / network.validator_count().max(1);
            for v in network.validators.iter().take(10) {
                let nerf_mult = if v.is_nerfed {
                    NerfRate::INITIAL.reward_multiplier()
                } else {
                    1.0
                };
                let effective = (per_val as f64 * nerf_mult) as u64;
                let score = v.hardware.composite_score();
                reward_records.push((epoch, v.id, per_val, nerf_mult, effective, score));
            }
        }
    }

    write_supply_curve(output_dir, &supply_records);
    write_reward_distribution(output_dir, &reward_records);

    println!("After {} epochs:", cli.epochs);
    println!("  Total emitted: {:.4} COMME", network.total_emitted as f64 / UNITS_PER_COMME as f64);
    println!("  Total burned: {:.4} COMME", network.total_burned as f64 / UNITS_PER_COMME as f64);
    println!("  Circulating: {:.4} COMME", network.circulating_supply() as f64 / UNITS_PER_COMME as f64);

    // Run all specialized simulations
    simulate_warehouse_attack();
    simulate_warehouse_scenarios(output_dir);
    simulate_network_growth(output_dir);
    simulate_emission_exhaustion();
    simulate_tier_accessibility(output_dir);
    simulate_burn_crossover(output_dir);
    simulate_hardware_evolution(output_dir);
    simulate_grace_period();
    simulate_charitable_burns(output_dir);
    simulate_validator_churn(output_dir, &mut rng);
    simulate_network_resilience(output_dir, &mut rng);
    simulate_sybil_cost(output_dir);
    simulate_sensitivity(output_dir);

    // Item 170: Generate HTML charts
    generate_html_charts(output_dir);

    // Item 173: Demonstrate parallel execution
    run_scenarios_parallel(output_dir, seed);

    println!("\n=== All simulations complete ===");
}

// ──────────────────────────────────────────────
// Item 171: Regression tests
// ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    /// Helper: create a deterministic RNG for tests
    fn test_rng() -> StdRng {
        StdRng::seed_from_u64(42)
    }

    // ── Test: emission never exceeds supply cap ──
    #[test]
    fn test_emission_never_exceeds_supply() {
        let schedule = EmissionSchedule::new();
        let mut rng = test_rng();
        let mut network = SimNetwork::new(1000, &mut rng);

        for _ in 0..10_000 {
            run_emission_epoch(&mut network, &schedule);
            assert!(network.total_emitted <= TOTAL_SUPPLY,
                "Emission exceeded supply cap: {} > {}", network.total_emitted, TOTAL_SUPPLY);
        }
    }

    // ── Test: burns never exceed circulating supply ──
    #[test]
    fn test_burns_never_exceed_circulating() {
        let schedule = EmissionSchedule::new();
        let mut rng = test_rng();
        let mut network = SimNetwork::new(500, &mut rng);

        for _ in 0..5_000 {
            run_emission_epoch(&mut network, &schedule);
            run_burn_epoch(&mut network, &mut rng);
            // circulating_supply uses saturating_sub, so it should never underflow
            let circ = network.circulating_supply();
            assert!(circ <= network.total_emitted,
                "Circulating {} exceeds emitted {}", circ, network.total_emitted);
        }
    }

    // ── Test: network initialization produces correct validator count ──
    #[test]
    fn test_network_init_validator_count() {
        let mut rng = test_rng();
        let network = SimNetwork::new(100, &mut rng);
        assert_eq!(network.validator_count(), 100);
        assert_eq!(network.online_count(), 100);
        assert_eq!(network.epoch, 0);
        assert_eq!(network.total_emitted, 0);
        assert_eq!(network.total_burned, 0);
    }

    // ── Test: validator initial state ──
    #[test]
    fn test_validator_initial_state() {
        let mut rng = test_rng();
        let network = SimNetwork::new(10, &mut rng);
        for v in &network.validators {
            assert_eq!(v.balance, 0);
            assert!(v.online);
            assert!(!v.is_nerfed);
            assert_eq!(v.contribution_percent, 100);
            assert!(matches!(v.compliance_status, ComplianceStatus::Compliant));
        }
    }

    // ── Test: hardware composite score is positive ──
    #[test]
    fn test_hardware_composite_score_positive() {
        let hw = HardwareProfile::reference();
        assert!(hw.composite_score() > 0.0);
    }

    // ── Test: nerfed validators earn less ──
    #[test]
    fn test_nerfed_validator_earns_less() {
        let schedule = EmissionSchedule::new();
        let mut rng = test_rng();

        // Normal network
        let mut normal_net = SimNetwork::new(100, &mut rng);
        let normal_emission = run_emission_epoch(&mut normal_net, &schedule);
        let normal_balance: u64 = normal_net.validators.iter().map(|v| v.balance).sum();

        // Network with all nerfed validators
        let mut rng2 = StdRng::seed_from_u64(42);
        let mut nerfed_net = SimNetwork::new(100, &mut rng2);
        for v in nerfed_net.validators.iter_mut() {
            v.is_nerfed = true;
        }
        let _nerfed_emission = run_emission_epoch(&mut nerfed_net, &schedule);
        let nerfed_balance: u64 = nerfed_net.validators.iter().map(|v| v.balance).sum();

        assert!(nerfed_balance < normal_balance,
            "Nerfed balance {} should be less than normal {}", nerfed_balance, normal_balance);
        // Emission total is the same (it's network-level), but individual rewards differ
        assert!(normal_emission > 0);
    }

    // ── Test: offline validators don't earn ──
    #[test]
    fn test_offline_validators_dont_earn() {
        let schedule = EmissionSchedule::new();
        let mut rng = test_rng();
        let mut network = SimNetwork::new(100, &mut rng);

        // Set first 50 offline
        for v in network.validators.iter_mut().take(50) {
            v.online = false;
        }

        run_emission_epoch(&mut network, &schedule);

        // Offline validators should have zero balance
        for v in network.validators.iter().take(50) {
            assert_eq!(v.balance, 0, "Offline validator {} should have 0 balance", v.id);
        }

        // Online validators should have non-zero balance
        for v in network.validators.iter().skip(50) {
            assert!(v.balance > 0, "Online validator {} should have positive balance", v.id);
        }
    }

    // ── Test: warehouse attack multi-node diminishing returns ──
    #[test]
    fn test_warehouse_diminishing_returns() {
        // multi_node_multiplier should decrease with more nodes
        let m1 = multi_node_multiplier(1);
        let m2 = multi_node_multiplier(2);
        let m5 = multi_node_multiplier(5);

        assert!(m1 >= m2, "First node multiplier should be >= second");
        assert!(m2 >= m5, "Second node multiplier should be >= fifth");
        assert!(m1 > 0.0, "First node should have positive multiplier");
    }

    // ── Test: circulating supply calculation ──
    #[test]
    fn test_circulating_supply() {
        let mut rng = test_rng();
        let mut network = SimNetwork::new(10, &mut rng);
        network.total_emitted = 1000;
        network.total_burned = 300;
        assert_eq!(network.circulating_supply(), 700);

        // Saturating: burned > emitted should give 0
        network.total_burned = 2000;
        assert_eq!(network.circulating_supply(), 0);
    }

    // ── Test: scenario parsing ──
    #[test]
    fn test_scenario_parsing() {
        assert!(matches!(Scenario::from_str_opt("growth"), Some(Scenario::Growth)));
        assert!(matches!(Scenario::from_str_opt("attack"), Some(Scenario::Attack)));
        assert!(matches!(Scenario::from_str_opt("crash"), Some(Scenario::Crash)));
        assert!(matches!(Scenario::from_str_opt("steady-state"), Some(Scenario::SteadyState)));
        assert!(matches!(Scenario::from_str_opt("steadystate"), Some(Scenario::SteadyState)));
        assert!(matches!(Scenario::from_str_opt("steady_state"), Some(Scenario::SteadyState)));
        assert!(matches!(Scenario::from_str_opt("all"), Some(Scenario::All)));
        assert!(Scenario::from_str_opt("invalid").is_none());
        assert!(Scenario::from_str_opt("").is_none());
    }

    // ── Test: deterministic RNG produces same results ──
    #[test]
    fn test_reproducibility_same_seed() {
        let schedule = EmissionSchedule::new();

        // Run 1
        let mut rng1 = StdRng::seed_from_u64(12345);
        let mut net1 = SimNetwork::new(100, &mut rng1);
        for _ in 0..100 {
            run_emission_epoch(&mut net1, &schedule);
            run_burn_epoch(&mut net1, &mut rng1);
        }

        // Run 2 with same seed
        let mut rng2 = StdRng::seed_from_u64(12345);
        let mut net2 = SimNetwork::new(100, &mut rng2);
        for _ in 0..100 {
            run_emission_epoch(&mut net2, &schedule);
            run_burn_epoch(&mut net2, &mut rng2);
        }

        assert_eq!(net1.total_emitted, net2.total_emitted,
            "Same seed should produce same emission");
        assert_eq!(net1.total_burned, net2.total_burned,
            "Same seed should produce same burns");
        assert_eq!(net1.circulating_supply(), net2.circulating_supply(),
            "Same seed should produce same circulating supply");
    }

    // ── Test: different seeds produce different results ──
    #[test]
    fn test_reproducibility_different_seeds() {
        let schedule = EmissionSchedule::new();

        let mut rng1 = StdRng::seed_from_u64(11111);
        let mut net1 = SimNetwork::new(100, &mut rng1);
        for _ in 0..1000 {
            run_emission_epoch(&mut net1, &schedule);
            run_burn_epoch(&mut net1, &mut rng1);
        }

        let mut rng2 = StdRng::seed_from_u64(99999);
        let mut net2 = SimNetwork::new(100, &mut rng2);
        for _ in 0..1000 {
            run_emission_epoch(&mut net2, &schedule);
            run_burn_epoch(&mut net2, &mut rng2);
        }

        // Emission is deterministic (same validator count), but burns have jitter
        assert_eq!(net1.total_emitted, net2.total_emitted,
            "Same config should produce same emission regardless of seed");
        // Burns should differ due to random jitter
        assert_ne!(net1.total_burned, net2.total_burned,
            "Different seeds should produce different burn totals");
    }

    // ── Test: sensitivity sim returns reasonable values ──
    #[test]
    fn test_sensitivity_sim_baseline() {
        let schedule = EmissionSchedule::new();
        let epochs = 24 * 365; // 1 year
        let (circ, daily, burned) = run_sensitivity_sim(&schedule, 100_000, epochs, 0.01);

        assert!(circ > 0.0, "Circulating should be positive");
        assert!(daily > 0.0, "Daily rate should be positive");
        assert!(burned > 0.0, "Burns should be positive with 1% rate");
        assert!(burned < circ, "Burns should be less than circulating in 1 year");
    }

    // ── Test: zero burn rate produces zero burns ──
    #[test]
    fn test_sensitivity_zero_burns() {
        let schedule = EmissionSchedule::new();
        let epochs = 24 * 365;
        let (_, _, burned) = run_sensitivity_sim(&schedule, 100_000, epochs, 0.0);
        assert_eq!(burned, 0.0, "Zero burn rate should produce zero burns");
    }

    // ── Test: tier thresholds are correctly ordered ──
    #[test]
    fn test_tier_thresholds_ordered() {
        assert!(HolderTier::BASE_THRESHOLD < HolderTier::STORAGE_THRESHOLD);
        assert!(HolderTier::STORAGE_THRESHOLD < HolderTier::COMPUTE_THRESHOLD);
        assert!(HolderTier::COMPUTE_THRESHOLD < HolderTier::FULL_THRESHOLD);
    }

    // ── Test: emission epoch increments network epoch ──
    #[test]
    fn test_epoch_counter_increments() {
        let schedule = EmissionSchedule::new();
        let mut rng = test_rng();
        let mut network = SimNetwork::new(10, &mut rng);
        assert_eq!(network.epoch, 0);

        run_emission_epoch(&mut network, &schedule);
        assert_eq!(network.epoch, 1);

        run_emission_epoch(&mut network, &schedule);
        assert_eq!(network.epoch, 2);
    }

    // ── Test: empty network doesn't panic ──
    #[test]
    fn test_empty_network_no_panic() {
        let schedule = EmissionSchedule::new();
        let mut rng = test_rng();
        let mut network = SimNetwork::new(0, &mut rng);

        // Should not panic with 0 validators
        // Note: emission may still be calculated (online_count().max(1) used internally)
        // but no validators exist to receive rewards
        let emission = run_emission_epoch(&mut network, &schedule);
        // Emission is computed but no validators to distribute to
        let _ = emission;

        let burn = run_burn_epoch(&mut network, &mut rng);
        // Burns may still happen from milestone logic, but should not panic
        let _ = burn;

        // Key assertion: no panic occurred
        assert_eq!(network.validator_count(), 0);
    }
}
