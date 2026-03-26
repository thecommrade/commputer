# Commputer Economic Simulator

The economic simulator (`commputer-sim`) models the Commputer network's tokenomics across various scenarios, including emission schedules, burn mechanics, validator churn, attack resistance, and long-term supply dynamics.

## Building and Running

```bash
cd src/
cargo build -p commputer-sim --release

# Basic run with defaults
./target/release/commputer-sim

# Custom parameters
./target/release/commputer-sim --validators 50000 --epochs 5000 --output results/ --seed 42
```

## CLI Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `--validators` | 10000 | Number of validators in the simulated network |
| `--epochs` | 1000 | Number of hourly epochs to simulate in the main loop |
| `--output` | `results` | Output directory for CSV files and HTML charts |
| `--scenario` | (none) | Pre-defined scenario to run: `growth`, `attack`, `crash`, `steady-state`, `all` |
| `--seed` | (system time) | Random seed for reproducible results. Same seed always produces identical output |

## Scenarios (`--scenario`)

| Scenario | Description | Simulations Run |
|----------|-------------|-----------------|
| `growth` | Network adoption and validator economics over time | Network growth (S-curve), validator churn |
| `attack` | Attack resistance analysis | Warehouse attack, warehouse scenarios, Sybil cost |
| `crash` | Network failure and recovery | Network resilience (30% crash, recovery modeling) |
| `steady-state` | Long-term economic equilibrium | Sensitivity analysis, burn crossover |
| `all` | Run everything (default when no scenario specified) | All simulations |

## Simulations

### Main Emission + Burn Loop
Runs `--epochs` iterations of the core emission and burn cycle. Each epoch:
- Calculates emission based on the hybrid curve and distributes to online validators
- Applies burn mechanics: burst compute burns (2% of validators), milestone burns (at 25%/50%/75% supply), and charitable burns (1%/year)
- Produces `supply_curve.csv` and `reward_distribution.csv`

### Warehouse Attack (`simulate_warehouse_attack`)
Models a single attacker running 100 nodes on one /24 subnet. Shows how `multi_node_multiplier` diminishing returns limit warehouse profitability.

### Warehouse Scenarios (`simulate_warehouse_scenarios`)
Three attacker strategies at multiple network sizes (1K, 10K, 100K):
- **Many small nodes**: 200 nodes on a single subnet
- **Few large nodes**: 5 nodes on different subnets (optimal per-node reward)
- **Geographic clustering**: 50 nodes across 5 subnets (10 per subnet)

Output: `warehouse_scenarios.csv`

### Network Growth (`simulate_network_growth`)
Models 10 years of network growth using a logistic (S-curve) function:
- K = 1,000,000 validators (carrying capacity)
- Midpoint at 5 years
- Tracks per-validator daily rate and cumulative rewards

Output: `network_growth.csv`

### Emission Exhaustion (`simulate_emission_exhaustion`)
Determines when the 2B COMME supply cap is reached at steady-state (100K validators). Verifies the floor rate (0.01 COMME/day) is always respected.

### Tier Accessibility (`simulate_tier_accessibility`)
Calculates time-to-tier for new validators at various network sizes and token prices:
- Tiers: Base (1 COMME), Storage (10), Compute (20), Full (33)
- Network sizes: 1K, 10K, 100K, 1M
- Token prices: $0.10, $1.00, $10.00, $100.00

Output: `tier_accessibility.csv`

### Burn Crossover (`simulate_burn_crossover`)
Finds the epoch where annual burns exceed annual emission. Models charitable burns (1%/year) and burst compute burns across 100 years.

Output: `burn_crossover.csv`

### Hardware Evolution (`simulate_hardware_evolution`)
Projects hardware capabilities over 10 years using Moore's Law (doubling every 2 years). Includes cost modeling: as hardware improves, the cost-per-performance decreases, making the gold standard more accessible.

Output: `hardware_evolution.csv`

### Grace Period (`simulate_grace_period`)
Validates the grace period system under four uptime patterns:
- 100% uptime, 80% uptime, 50% uptime, weekend-only
- Drain: 1:1 (1 hour offline = 1 hour consumed)
- Refill: 2:1 (2 hours online = 1 hour restored)
- Cap: 87,600 hours (10 years)

### Charitable Burns (`simulate_charitable_burns`)
Models charitable burn impact at multiple adoption levels (1K to 1M validators), time horizons (1 to 50 years), and burn rates (0.5%, 1%, 2%).

Output: `charitable_burns.csv`

### Validator Churn (`simulate_validator_churn`)
Models validators joining and leaving over 5 years:
- Monthly join rate: 5%
- Monthly leave rate: 3% (with random variation)
- Tracks impact on per-validator emission rate

Output: `validator_churn.csv`

### Network Resilience (`simulate_network_resilience`)
Simulates a 30% validator crash at epoch 100 with gradual recovery (2% per epoch). Tracks:
- Online validator count and percentage
- Per-validator emission rate changes
- Time to recover to 95%+ online

Output: `network_resilience.csv`

### Sybil Cost Analysis (`simulate_sybil_cost`)
Calculates attack costs at various network sizes and attack percentages (10%, 20%, 33%, 51%):
- Hardware cost: $800/node
- Monthly operating cost: $50/node
- Breakeven analysis at $1/COMME

Output: `sybil_cost.csv`

### Sensitivity Analysis (`simulate_sensitivity`)
Varies key parameters to identify which matter most:
- **Validator count**: 0.5x to 10x baseline
- **Burn rate**: 0% to 5%
- Metrics: circulating supply, per-validator daily rate, total burned

Output: `sensitivity.csv`

### HTML Visualization (`generate_html_charts`)
Generates a self-contained HTML file with Chart.js visualizations for supply curve, burn crossover, network growth, tier accessibility, and validator churn data.

Output: `charts.html`

## Output Files

All CSV files are written to the `--output` directory:

| File | Contents |
|------|----------|
| `supply_curve.csv` | Epoch-by-epoch emission, burn, and circulating supply |
| `reward_distribution.csv` | Per-validator reward samples (every 100 epochs) |
| `warehouse_scenarios.csv` | Attack strategy comparison across network sizes |
| `network_growth.csv` | S-curve growth with emission rate over 10 years |
| `tier_accessibility.csv` | Time-to-tier at various sizes and prices |
| `burn_crossover.csv` | Annual emission vs burn over 100 years |
| `hardware_evolution.csv` | Hardware capability and cost projections |
| `charitable_burns.csv` | Charitable burn totals by adoption and rate |
| `validator_churn.csv` | Validator count and churn rate over 5 years |
| `network_resilience.csv` | Crash and recovery dynamics |
| `sybil_cost.csv` | Attack cost analysis |
| `sensitivity.csv` | Parameter sensitivity results |
| `charts.html` | Interactive HTML charts |

## Reproducibility

Use `--seed <value>` to get deterministic results. The simulator uses `StdRng::seed_from_u64` for all random number generation. The same seed with the same parameters will always produce identical CSV output.

```bash
# These two runs produce identical output:
./target/release/commputer-sim --seed 42 --output run1/
./target/release/commputer-sim --seed 42 --output run2/
```

## Parallel Execution

When running all scenarios (default or `--scenario all`), the simulator executes independent scenarios in parallel using OS threads. Each parallel scenario gets its own output subdirectory:
- `scenario_growth/`
- `scenario_attack/`
- `scenario_crash/`
- `scenario_steady/`
