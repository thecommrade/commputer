use commputer_core::identity::Address;
use commputer_core::proof::EpochProofSummary;
use sha2::{Sha256, Digest};

/// Selects the anchor (block producer) for each round using VRF-weighted selection.
/// Weight is based on Composite Resource Score — not stake, not hash power.
/// This means block production probability correlates with actual resource contribution.
pub struct AnchorSelector;

impl AnchorSelector {
    /// Select an anchor from the validator set for a given round.
    ///
    /// Uses a deterministic VRF-like selection:
    /// 1. Hash(round_seed || validator_address) → per-validator ticket
    /// 2. Weight each ticket by composite resource score
    /// 3. Lowest weighted ticket wins
    ///
    /// This is deterministic given the same inputs, so all honest nodes
    /// agree on the anchor without communication.
    pub fn select(
        round_seed: &[u8; 32],
        validators: &[EpochProofSummary],
    ) -> Option<Address> {
        if validators.is_empty() {
            return None;
        }

        let mut best_address = None;
        let mut best_score = u64::MAX;

        for summary in validators {
            let composite = summary.composite_score();
            if composite == 0 {
                continue;
            }

            // Generate deterministic ticket for this validator + round.
            let mut hasher = Sha256::new();
            hasher.update(round_seed);
            hasher.update(summary.validator.0);
            let ticket_hash = hasher.finalize();

            // Take first 8 bytes as u64.
            let ticket = u64::from_le_bytes(
                ticket_hash[..8].try_into().unwrap()
            );

            // Weight: lower ticket / higher composite score = better chance.
            // This ensures higher contributors are more likely to produce blocks.
            let weighted = ticket / composite;

            if weighted < best_score {
                best_score = weighted;
                best_address = Some(summary.validator);
            }
        }

        best_address
    }

    /// Generate the round seed from the previous block hash and round number.
    pub fn round_seed(prev_block_hash: &[u8; 32], round: u64) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(prev_block_hash);
        hasher.update(round.to_le_bytes());
        let result = hasher.finalize();
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&result);
        seed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_core::proof::EpochProofSummary;

    fn make_summary(id: u8, score: u32) -> EpochProofSummary {
        let mut addr = [0u8; 32];
        addr[0] = id;
        EpochProofSummary {
            validator: Address(addr),
            epoch: 0,
            processing_score: score,
            gpu_score: score,
            storage_score: score,
            ram_score: score,
            bandwidth_score: score,
            diversity_bonus: 50,
        }
    }

    #[test]
    fn deterministic_selection() {
        let seed = [42u8; 32];
        let validators = vec![
            make_summary(1, 100),
            make_summary(2, 100),
            make_summary(3, 100),
        ];

        let a = AnchorSelector::select(&seed, &validators);
        let b = AnchorSelector::select(&seed, &validators);
        assert_eq!(a, b); // Same inputs → same output.
    }

    #[test]
    fn empty_validators_returns_none() {
        let seed = [0u8; 32];
        assert_eq!(AnchorSelector::select(&seed, &[]), None);
    }

    #[test]
    fn higher_score_wins_more_often() {
        // Over many rounds, the high-score validator should win more.
        let validators = vec![
            make_summary(1, 10),    // Low score
            make_summary(2, 1000),  // High score
        ];

        let mut wins = [0u32; 2];
        for round in 0u64..1000 {
            let seed = AnchorSelector::round_seed(&[0u8; 32], round);
            if let Some(winner) = AnchorSelector::select(&seed, &validators) {
                if winner.0[0] == 1 { wins[0] += 1; }
                if winner.0[0] == 2 { wins[1] += 1; }
            }
        }

        // High-score validator should win significantly more.
        assert!(wins[1] > wins[0],
            "High-score wins: {}, Low-score wins: {}", wins[1], wins[0]);
    }
}
