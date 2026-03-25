# Commputer Tokenomics

## Token: $COMME

- **Total supply**: 2,000,000,000 (2 billion)
- **Decimals**: 8 (1 COMME = 100,000,000 raw units)
- **No pre-mine, no ICO** -- all tokens are mined through resource contribution

## Emission Schedule

### Base Rate
- A single maxed reference node earns **0.09 COMME/day** at launch
- Approximately **32.85 COMME/year** (the "gold standard" rate)
- One year of full contribution earns enough for the Full tier (33 COMME)

### Hybrid Curve
- Below 10,000 validators: flat rate (0.09 COMME/day/validator)
- Above 10,000 validators: inverse square root scaling
  - `rate = base_rate * sqrt(10000 / validator_count)`
- Floor rate: **0.01 COMME/day** regardless of network size

### Per-Epoch Emission
- Epochs last 1 hour (3600 seconds)
- Per-epoch emission = daily emission / 24
- Distributed proportionally to validators' Composite Resource Scores

### Channel Allocation
Emission is split across 5 resource channels with guaranteed floors:

| Channel | Floor | Basis Points |
|---|---|---|
| Processing (CPU) | 10% | 1000 |
| GPU | 10% | 1000 |
| Storage | 10% | 1000 |
| RAM | 5% | 500 |
| Bandwidth | 5% | 500 |
| **Demand-weighted surplus** | **60%** | **6000** |

The remaining 60% is distributed proportional to network demand per channel. If no demand signal exists, it splits equally.

## Burn Mechanics

### Fee Burns
- Minimum fee: 100,000 raw units (0.001 COMME)
- All transaction fees are **burned**, not paid to validators
- Validators earn through emission only

### Burst Compute Burns
- Users burn $COMME to purchase burst compute beyond tier allocation
- Burned amount is permanent and deflationary

### Milestone Burns
- Protocol-triggered burns at capacity/adoption milestones
- Three tiers: capacity (hardcoded), adoption (seasonal), utility (organic)

### Charitable Burns
- Annual vote: protocol sells $COMME for the chosen cause AND burns a matching amount

## Holder Tiers

| Tier | Threshold | Access |
|---|---|---|
| None | 0 COMME | No access |
| Base | 1 COMME | Full analytics platform |
| Storage | 10 COMME | Communal storage allocation |
| Compute | 20 COMME | Communal compute allocation |
| Full | 33 COMME | Full personal computer + AI |

### Storage Allocations by Tier
| Tier | Allocation |
|---|---|
| None | 0 |
| Base | 1 GB |
| Storage | 10 GB |
| Compute | 20 GB |
| Full | 50 GB |

### Emergency Access
When remaining supply drops below 1,000,000 COMME, any contribution grants full access.

## Fee Structure

- Minimum transaction fee: 0.001 COMME (100,000 raw units)
- All fees are burned (deflationary)
- No gas price auction -- flat minimum fee

## Anti-Scale Economics

### Nerf Rate
- Starting nerf: **80%** reward reduction for non-compliant validators
- Adaptive: scales up to **100%** based on nerfed-to-total ratio
- Formula: `8000 + (nerfed_ratio * 2000)` basis points
- Can only increase, never decrease

### Multi-Node Decay
| Node # | Reward Multiplier |
|---|---|
| 1 | 100% |
| 2 | 25% |
| 3 | 6.25% |
| 4 | 1.5625% |
| 5+ | 0% |

100 nerfed validators in a warehouse earn less total than a single honest desktop.

### Resource Score Sub-Linearity
- Per-channel score uses R^0.7 formula
- Doubling resources gives ~1.62x the score, not 2x
- Prevents hardware arms races

## Grace Period

- Contributors earn 1 second of grace per 1 second of uptime (capped at 10 years)
- Grace drains 1:1 while offline
- Grace refills 2:1 while online (5 days online restores 10 days of grace)

## Supply Projections

At various network sizes (first year):

| Validators | Daily/Validator | Annual/Validator | Total Annual |
|---|---|---|---|
| 1,000 | 0.09 COMME | 32.85 COMME | 32,850 COMME |
| 10,000 | 0.09 COMME | 32.85 COMME | 328,500 COMME |
| 100,000 | 0.028 COMME | 10.39 COMME | 1,039,230 COMME |
| 1,000,000 | 0.01 COMME | 3.65 COMME | 3,650,000 COMME |
