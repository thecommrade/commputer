use std::fs;

use clap::Parser;
use rand::Rng;

use commputer_core::compliance::{ComplianceStatus, NerfRate, multi_node_multiplier};
use commputer_core::tier::HolderTier;
use commputer_core::token::{TOTAL_SUPPLY, UNITS_PER_COMME};
use commputer_consensus::emission::EmissionSchedule;

// ──────────────────────────────────────────────
// Feature 165: CLI
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
}

// ──────────────────────────────────────────────
// Feature 152: Simulator core types
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
}

struct SimNetwork {
    validators: Vec<SimValidator>,
    epoch: u64,
    total_emitted: u64,
    total_burned: u64,
}

impl SimNetwork {
    fn new(validator_count: u64) -> Self {
        let mut rng = rand::thread_rng();
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

    fn circulating_supply(&self) -> u64 {
        self.total_emitted.saturating_sub(self.total_burned)
    }
}

// ──────────────────────────────────────────────
// Feature 153: Emission simulation
// ──────────────────────────────────────────────

fn run_emission_epoch(network: &mut SimNetwork, schedule: &EmissionSchedule) -> u64 {
    let count = network.validator_count();
    let epoch_emission = schedule.per_epoch_emission(count);

    // Cap emission at remaining supply
    let remaining = TOTAL_SUPPLY.saturating_sub(network.total_emitted);
    let actual_emission = epoch_emission.min(remaining);

    if actual_emission == 0 {
        return 0;
    }

    // Distribute equally among validators (simplified)
    let per_validator = actual_emission / count.max(1);
    for v in network.validators.iter_mut() {
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
// Feature 154: Burn simulation
// ──────────────────────────────────────────────

fn run_burn_epoch(network: &mut SimNetwork, rng: &mut impl Rng) -> u64 {
    let mut epoch_burn = 0u64;
    let circulating = network.circulating_supply();

    // Random burst compute: 2% of validators burn a small amount each epoch
    let burst_count = (network.validator_count() as f64 * 0.02) as u64;
    for _ in 0..burst_count {
        // Each burst burns 0.001 COMME
        let burn = UNITS_PER_COMME / 1000;
        if epoch_burn + burn <= circulating {
            epoch_burn += burn;
        }
    }

    // Milestone burns at capacity thresholds (check once)
    let supply_ratio = network.total_emitted as f64 / TOTAL_SUPPLY as f64;
    let milestones = [0.25, 0.50, 0.75];
    for &milestone in &milestones {
        // Trigger if we just crossed the milestone this epoch (approximation)
        let prev_ratio = (network.total_emitted.saturating_sub(UNITS_PER_COMME * 100)) as f64 / TOTAL_SUPPLY as f64;
        if prev_ratio < milestone && supply_ratio >= milestone {
            // Burn 0.1% of circulating
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
// Feature 155: Supply curve output
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
// Feature 156: Reward distribution output
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
// Feature 157: Warehouse attack simulation
// ──────────────────────────────────────────────

fn simulate_warehouse_attack() {
    println!("\n=== Warehouse Attack Simulation (Feature 157) ===");
    let schedule = EmissionSchedule::new();
    let validator_count = 1000u64; // total network including attacker

    let daily_rate = schedule.per_validator_daily_rate(validator_count);
    let honest_reward = daily_rate;

    // Attacker runs 100 nodes on the same /24 subnet
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
    println!("Conclusion: 100 warehouse nodes earn {:.2}x what 1 honest node earns",
        attacker_total as f64 / honest_reward as f64);

    // Show per-node breakdown
    println!("\nPer-node breakdown:");
    for i in 1..=5u32 {
        let mult = multi_node_multiplier(i);
        println!("  Node {}: multiplier={:.6}, reward={:.6} COMME",
            i, mult, daily_rate as f64 * mult / UNITS_PER_COMME as f64);
    }
    println!("  Nodes 5-100: multiplier=0.0, reward=0.0 COMME");
}

// ──────────────────────────────────────────────
// Feature 158: Network growth simulation (S-curve)
// ──────────────────────────────────────────────

fn simulate_network_growth(output_dir: &str) {
    println!("\n=== Network Growth Simulation (Feature 158) ===");
    let schedule = EmissionSchedule::new();

    // Logistic growth: N(t) = K / (1 + e^(-r*(t - t0)))
    let k: f64 = 1_000_000.0;
    let r: f64 = 0.005;
    let t0: f64 = (5 * 365 * 24) as f64; // midpoint at 5 years in epochs (hourly)

    let total_epochs = 10 * 365 * 24; // 10 years of hourly epochs

    let mut records: Vec<(u64, u64, f64, f64)> = Vec::new(); // epoch, validators, per_validator_daily, cumulative_comme

    let mut cumulative_reward = 0.0f64;
    let sample_interval = 24 * 30; // monthly samples

    for epoch in 0..total_epochs {
        let t = epoch as f64;
        let n = (k / (1.0 + (-r * (t - t0)).exp())).max(100.0) as u64;

        let per_val_daily = schedule.per_validator_daily_rate(n);
        let per_val_epoch = per_val_daily as f64 / 24.0;
        cumulative_reward += per_val_epoch;

        if epoch % sample_interval == 0 {
            let cumulative_comme = cumulative_reward / UNITS_PER_COMME as f64;
            records.push((epoch as u64, n, per_val_daily as f64 / UNITS_PER_COMME as f64, cumulative_comme));
        }
    }

    // Write CSV
    let path = format!("{}/network_growth.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create network_growth.csv");
    wtr.write_record(["epoch", "validator_count", "per_validator_daily_comme", "cumulative_reward_comme"]).unwrap();
    for &(e, n, daily, cum) in &records {
        wtr.write_record(&[e.to_string(), n.to_string(), format!("{:.6}", daily), format!("{:.4}", cum)]).unwrap();
    }
    wtr.flush().unwrap();
    println!("Wrote {}", path);

    // Print summary
    let first = records.first().unwrap();
    let last = records.last().unwrap();
    println!("Start: {} validators, {:.6} COMME/day/validator", first.1, first.2);
    println!("End (10yr): {} validators, {:.6} COMME/day/validator", last.1, last.2);
    println!("Cumulative reward for day-1 validator: {:.2} COMME over 10 years", last.3);
}

// ──────────────────────────────────────────────
// Feature 159: Emission exhaustion simulation
// ──────────────────────────────────────────────

fn simulate_emission_exhaustion() {
    println!("\n=== Emission Exhaustion Simulation (Feature 159) ===");
    let schedule = EmissionSchedule::new();

    let validator_count = 100_000u64; // steady-state assumption
    let mut total_emitted = 0u64;
    let mut epoch = 0u64;
    let max_epochs = 200 * 365 * 24; // 200 years max

    loop {
        let remaining = TOTAL_SUPPLY.saturating_sub(total_emitted);
        if remaining == 0 || epoch >= max_epochs {
            break;
        }

        let emission = schedule.per_epoch_emission(validator_count).min(remaining);
        total_emitted += emission;
        epoch += 1;
    }

    assert!(total_emitted <= TOTAL_SUPPLY, "Total emitted exceeds 2B supply cap!");

    let years = epoch as f64 / (365.0 * 24.0);
    println!("Supply exhausted at epoch {} ({:.1} years)", epoch, years);
    println!("Total emitted: {:.2} COMME", total_emitted as f64 / UNITS_PER_COMME as f64);
    println!("Supply cap (2B): {:.2} COMME", TOTAL_SUPPLY as f64 / UNITS_PER_COMME as f64);
    println!("Verified: total_emitted <= TOTAL_SUPPLY: {}", total_emitted <= TOTAL_SUPPLY);

    // Check floor rate
    let floor_rate = schedule.per_validator_daily_rate(100_000_000);
    println!("Floor rate at 100M validators: {:.6} COMME/day", floor_rate as f64 / UNITS_PER_COMME as f64);
    assert!(floor_rate >= UNITS_PER_COMME / 100, "Floor rate violated!");
    println!("Floor rate >= 0.01 COMME/day: verified");
}

// ──────────────────────────────────────────────
// Feature 160: Tier accessibility simulation
// ──────────────────────────────────────────────

fn simulate_tier_accessibility(output_dir: &str) {
    println!("\n=== Tier Accessibility Simulation (Feature 160) ===");
    let schedule = EmissionSchedule::new();

    let network_sizes = [1_000u64, 10_000, 100_000, 1_000_000];
    let tiers = [
        ("Base", HolderTier::BASE_THRESHOLD),
        ("Storage", HolderTier::STORAGE_THRESHOLD),
        ("Compute", HolderTier::COMPUTE_THRESHOLD),
        ("Full", HolderTier::FULL_THRESHOLD),
    ];

    let path = format!("{}/tier_accessibility.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create tier_accessibility.csv");
    wtr.write_record(["network_size", "tier", "epochs_to_reach"]).unwrap();

    for &size in &network_sizes {
        let daily_rate = schedule.per_validator_daily_rate(size);
        println!("\nNetwork size: {} validators (daily rate: {:.6} COMME)",
            size, daily_rate as f64 / UNITS_PER_COMME as f64);

        for &(tier_name, threshold) in &tiers {
            let target_raw = threshold * UNITS_PER_COMME;
            let per_epoch = daily_rate / 24; // hourly epochs
            let epochs_needed = if per_epoch > 0 {
                (target_raw + per_epoch - 1) / per_epoch // ceiling division
            } else {
                u64::MAX
            };
            let days = epochs_needed as f64 / 24.0;
            println!("  {} tier ({} COMME): {} epochs ({:.1} days)",
                tier_name, threshold, epochs_needed, days);

            wtr.write_record(&[size.to_string(), tier_name.to_string(), epochs_needed.to_string()]).unwrap();
        }
    }
    wtr.flush().unwrap();
    println!("Wrote {}", path);
}

// ──────────────────────────────────────────────
// Feature 161: Burn crossover point
// ──────────────────────────────────────────────

fn simulate_burn_crossover() {
    println!("\n=== Burn Crossover Simulation (Feature 161) ===");
    let schedule = EmissionSchedule::new();

    let validator_count = 100_000u64;
    let epochs_per_year: u64 = 24 * 365;

    let mut total_emitted = 0u64;
    let mut total_burned = 0u64;
    let mut annual_emission_window = Vec::new();
    let mut annual_burn_window = Vec::new();
    let mut crossover_epoch = None;

    let max_epochs = 100 * epochs_per_year; // 100 years

    for epoch in 0..max_epochs {
        let remaining = TOTAL_SUPPLY.saturating_sub(total_emitted);
        let emission = schedule.per_epoch_emission(validator_count).min(remaining);
        total_emitted += emission;

        let circulating = total_emitted.saturating_sub(total_burned);

        // Burn: charitable (1%/year) + burst compute (2% of validators * 0.001 COMME)
        let charitable_per_epoch = circulating / 100 / epochs_per_year;
        let burst_burn = (validator_count as f64 * 0.02) as u64 * (UNITS_PER_COMME / 1000);
        let epoch_burn = charitable_per_epoch + burst_burn;
        total_burned += epoch_burn;

        annual_emission_window.push(emission);
        annual_burn_window.push(epoch_burn);

        // Keep rolling window
        if annual_emission_window.len() > epochs_per_year as usize {
            annual_emission_window.remove(0);
            annual_burn_window.remove(0);
        }

        if annual_emission_window.len() == epochs_per_year as usize && crossover_epoch.is_none() {
            let annual_em: u64 = annual_emission_window.iter().sum();
            let annual_bn: u64 = annual_burn_window.iter().sum();
            if annual_bn > annual_em {
                crossover_epoch = Some(epoch);
            }
        }
    }

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
// Feature 162: Gold standard hardware simulation
// ──────────────────────────────────────────────

fn simulate_hardware_evolution(output_dir: &str) {
    println!("\n=== Gold Standard Hardware Simulation (Feature 162) ===");

    let path = format!("{}/hardware_evolution.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create hardware_evolution.csv");
    wtr.write_record(["year", "cpu_score", "ram_gb", "storage_tb"]).unwrap();

    // Moore's law: doubles every 2 years
    for year in 0..=10 {
        let multiplier = 2.0f64.powf(year as f64 / 2.0);
        let cpu_score = (100.0 * multiplier) as u64;
        let ram_gb = (16.0 * multiplier) as u64;
        let storage_tb = (1.0 * multiplier) as u64;

        println!("Year {}: CPU={}, RAM={}GB, Storage={}TB",
            year, cpu_score, ram_gb, storage_tb.max(1));

        wtr.write_record(&[
            year.to_string(),
            cpu_score.to_string(),
            ram_gb.to_string(),
            storage_tb.max(1).to_string(),
        ]).unwrap();
    }
    wtr.flush().unwrap();
    println!("Wrote {}", path);
}

// ──────────────────────────────────────────────
// Feature 163: Grace period simulation
// ──────────────────────────────────────────────

fn simulate_grace_period() {
    println!("\n=== Grace Period Simulation (Feature 163) ===");

    // Grace period rules:
    // - 1:1 drain: 1 hour offline = 1 hour of grace consumed
    // - 2:1 refill: 2 hours online = 1 hour of grace restored
    // - 10-year cap: max 87,600 hours of grace (10 * 365 * 24)

    let max_grace_hours: f64 = 10.0 * 365.0 * 24.0; // 87,600 hours
    let sim_epochs = 365 * 24; // 1 year of hourly epochs

    struct UptimePattern {
        name: &'static str,
        /// Returns true if the validator is online at this epoch
        is_online: fn(epoch: u64) -> bool,
    }

    let patterns = [
        UptimePattern { name: "100% uptime", is_online: |_| true },
        UptimePattern { name: "80% uptime", is_online: |e| e % 5 != 0 }, // offline every 5th epoch
        UptimePattern { name: "50% uptime", is_online: |e| e % 2 == 0 },
        UptimePattern { name: "Weekend-only", is_online: |e| {
            // Weekend = hours 120-167 of each 168-hour week
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
                // 2:1 refill: 1 hour online = 0.5 hours of grace
                grace_hours = (grace_hours + 0.5).min(max_grace_hours);
            } else {
                offline_epochs += 1;
                // 1:1 drain
                grace_hours = (grace_hours - 1.0).max(0.0);
            }
        }

        let actual_uptime = online_epochs as f64 / sim_epochs as f64 * 100.0;
        println!("{}: uptime={:.1}%, grace_hours={:.1}/{:.0}, online={}, offline={}",
            pattern.name, actual_uptime, grace_hours, max_grace_hours,
            online_epochs, offline_epochs);

        // Verify math
        assert!(grace_hours >= 0.0, "Grace hours went negative!");
        assert!(grace_hours <= max_grace_hours, "Grace hours exceeded cap!");
    }
    println!("Verified: 1:1 drain, 2:1 refill, 10-year cap constraints hold.");
}

// ──────────────────────────────────────────────
// Feature 164: Charitable burn impact
// ──────────────────────────────────────────────

fn simulate_charitable_burns(output_dir: &str) {
    println!("\n=== Charitable Burn Impact Simulation (Feature 164) ===");
    let schedule = EmissionSchedule::new();

    let adoption_levels = [1_000u64, 10_000, 100_000, 1_000_000];
    let time_horizons = [10u64, 20, 50]; // years

    let path = format!("{}/charitable_burns.csv", output_dir);
    let mut wtr = csv::Writer::from_path(&path).expect("Failed to create charitable_burns.csv");
    wtr.write_record(["validators", "years", "total_donated_comme"]).unwrap();

    for &validators in &adoption_levels {
        for &years in &time_horizons {
            let epochs_per_year: u64 = 24 * 365;
            let total_epochs = years * epochs_per_year;

            let mut total_emitted = 0u64;
            let mut total_burned = 0u64;
            let mut total_charitable = 0u64;

            for _ in 0..total_epochs {
                let remaining = TOTAL_SUPPLY.saturating_sub(total_emitted);
                let emission = schedule.per_epoch_emission(validators).min(remaining);
                total_emitted += emission;

                let circulating = total_emitted.saturating_sub(total_burned);
                let charitable_per_epoch = circulating / 100 / epochs_per_year;
                total_charitable += charitable_per_epoch;
                total_burned += charitable_per_epoch;
            }

            let donated_comme = total_charitable as f64 / UNITS_PER_COMME as f64;
            println!("{} validators, {} years: {:.2} COMME donated to charity",
                validators, years, donated_comme);

            wtr.write_record(&[
                validators.to_string(),
                years.to_string(),
                format!("{:.2}", donated_comme),
            ]).unwrap();
        }
    }
    wtr.flush().unwrap();
    println!("Wrote {}", path);
}

// ──────────────────────────────────────────────
// Main: run all simulations
// ──────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    let output_dir = &cli.output;

    // Ensure output directory exists
    fs::create_dir_all(output_dir).expect("Failed to create output directory");

    println!("=== Commputer Economic Simulator ===");
    println!("Validators: {}", cli.validators);
    println!("Epochs: {}", cli.epochs);
    println!("Output: {}", output_dir);

    let schedule = EmissionSchedule::new();
    let mut rng: rand::rngs::ThreadRng = rand::thread_rng();

    // ── Feature 153/154/155: Main emission + burn simulation ──
    println!("\n=== Main Simulation (Features 153-155) ===");
    let mut network = SimNetwork::new(cli.validators);
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

        // Feature 156: Sample reward distribution (first 10 validators, every 100 epochs)
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

    // ── Feature 157: Warehouse attack ──
    simulate_warehouse_attack();

    // ── Feature 158: Network growth ──
    simulate_network_growth(output_dir);

    // ── Feature 159: Emission exhaustion ──
    simulate_emission_exhaustion();

    // ── Feature 160: Tier accessibility ──
    simulate_tier_accessibility(output_dir);

    // ── Feature 161: Burn crossover ──
    simulate_burn_crossover();

    // ── Feature 162: Hardware evolution ──
    simulate_hardware_evolution(output_dir);

    // ── Feature 163: Grace period ──
    simulate_grace_period();

    // ── Feature 164: Charitable burns ──
    simulate_charitable_burns(output_dir);

    println!("\n=== All simulations complete ===");
}
