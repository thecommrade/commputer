use std::collections::HashMap;
use commputer_core::block::BlockHash;
use rand::seq::SliceRandom;

/// Snowball consensus parameters.
/// These control how quickly the network converges on a decision.
#[derive(Debug, Clone)]
pub struct SnowballParams {
    /// Number of peers to sample each round (k).
    pub sample_size: usize,
    /// Quorum threshold — how many of the sample must agree (α).
    /// Must be > k/2 for liveness.
    pub quorum: usize,
    /// Number of consecutive rounds a preference must hold to finalize (β).
    pub decision_threshold: u32,
}

impl Default for SnowballParams {
    fn default() -> Self {
        Self {
            sample_size: 20,
            quorum: 14,          // 70% of sample
            decision_threshold: 20, // 20 consecutive rounds
        }
    }
}

/// The state of a Snowball vote on a single decision (e.g., which block to accept).
#[derive(Debug, Clone)]
pub struct SnowballVoter {
    params: SnowballParams,
    /// Current preferred choice.
    preference: Option<BlockHash>,
    /// How many consecutive rounds the current preference has held.
    consecutive_count: u32,
    /// Confidence counters per choice — tracks total successful queries.
    confidence: HashMap<BlockHash, u32>,
    /// Whether a final decision has been reached.
    finalized: bool,
}

impl SnowballVoter {
    pub fn new(params: SnowballParams) -> Self {
        Self {
            params,
            preference: None,
            consecutive_count: 0,
            confidence: HashMap::new(),
            finalized: false,
        }
    }

    pub fn with_default_params() -> Self {
        Self::new(SnowballParams::default())
    }

    /// Whether this voter has reached a final decision.
    pub fn is_finalized(&self) -> bool {
        self.finalized
    }

    /// The current preferred block hash, if any.
    pub fn preference(&self) -> Option<BlockHash> {
        self.preference
    }

    /// The finalized block hash, if decided.
    pub fn finalized_hash(&self) -> Option<BlockHash> {
        if self.finalized {
            self.preference
        } else {
            None
        }
    }

    /// Select a random sample of peers to query.
    pub fn select_sample<'a>(
        &self,
        peers: &'a [BlockHash],
        rng: &mut impl rand::Rng,
    ) -> Vec<&'a BlockHash> {
        let k = self.params.sample_size.min(peers.len());
        let mut sample: Vec<&BlockHash> = peers.iter().collect();
        sample.shuffle(rng);
        sample.truncate(k);
        sample
    }

    /// Process the results of a sampling round.
    /// `responses` maps each choice to the count of peers who voted for it.
    /// Returns true if the decision was just finalized.
    pub fn record_round(&mut self, responses: &HashMap<BlockHash, usize>) -> bool {
        if self.finalized {
            return false;
        }

        // Find if any choice reached quorum in this sample.
        let quorum_choice = responses
            .iter()
            .find(|&(_, &count)| count >= self.params.quorum)
            .map(|(hash, _)| *hash);

        match quorum_choice {
            Some(choice) => {
                // Increment confidence for this choice.
                *self.confidence.entry(choice).or_insert(0) += 1;

                // Check if this is a new preference or continuation.
                if self.preference == Some(choice) {
                    self.consecutive_count += 1;
                } else {
                    // Switch preference to the choice with highest confidence.
                    let current_conf = self
                        .preference
                        .and_then(|p| self.confidence.get(&p).copied())
                        .unwrap_or(0);
                    let new_conf = self.confidence.get(&choice).copied().unwrap_or(0);

                    if new_conf >= current_conf {
                        self.preference = Some(choice);
                        self.consecutive_count = 1;
                    }
                }

                // Check if we've reached the decision threshold.
                if self.consecutive_count >= self.params.decision_threshold {
                    self.finalized = true;
                    return true;
                }
            }
            None => {
                // No quorum reached — reset consecutive counter.
                self.consecutive_count = 0;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(n: u8) -> BlockHash {
        let mut h = [0u8; 32];
        h[0] = n;
        BlockHash(h)
    }

    #[test]
    fn converges_on_unanimous_choice() {
        let params = SnowballParams {
            sample_size: 5,
            quorum: 4,
            decision_threshold: 3,
        };
        let mut voter = SnowballVoter::new(params);
        let choice = hash(1);

        // Simulate 3 unanimous rounds.
        let mut responses = HashMap::new();
        responses.insert(choice, 5);

        assert!(!voter.record_round(&responses));
        assert!(!voter.record_round(&responses));
        assert!(voter.record_round(&responses)); // 3rd consecutive → finalized

        assert!(voter.is_finalized());
        assert_eq!(voter.finalized_hash(), Some(choice));
    }

    #[test]
    fn no_finalization_without_quorum() {
        let params = SnowballParams {
            sample_size: 5,
            quorum: 4,
            decision_threshold: 3,
        };
        let mut voter = SnowballVoter::new(params);

        // Split vote — no one reaches quorum of 4.
        let mut responses = HashMap::new();
        responses.insert(hash(1), 3);
        responses.insert(hash(2), 2);

        for _ in 0..10 {
            voter.record_round(&responses);
        }
        assert!(!voter.is_finalized());
    }

    #[test]
    fn preference_switches_on_higher_confidence() {
        let params = SnowballParams {
            sample_size: 5,
            quorum: 4,
            decision_threshold: 5,
        };
        let mut voter = SnowballVoter::new(params);

        let a = hash(1);
        let b = hash(2);

        // 2 rounds favoring A.
        let mut resp_a = HashMap::new();
        resp_a.insert(a, 5);
        voter.record_round(&resp_a);
        voter.record_round(&resp_a);
        assert_eq!(voter.preference(), Some(a));

        // 3 rounds favoring B — B should take over.
        let mut resp_b = HashMap::new();
        resp_b.insert(b, 5);
        voter.record_round(&resp_b);
        voter.record_round(&resp_b);
        voter.record_round(&resp_b);
        assert_eq!(voter.preference(), Some(b));
    }
}
