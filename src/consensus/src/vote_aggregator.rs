//! Peer-keyed vote aggregation with k-sampling for Snowball rounds.
//!
//! WHAT: dedups incoming block-preference votes by peer so a single peer counts
//! at most once per `(height, block_hash)`, then k-samples the deduped voter set
//! before emitting the `HashMap<BlockHash, usize>` that
//! [`crate::snowball::SnowballVoter::record_round`] consumes.
//!
//! WHY: this is the LIVE vote-counting path. `ConsensusManager` holds one
//! `VoteAggregator<PeerId>` per height (a `HeightState` field) and feeds it via
//! `record_peer_response` -> `record_vote`; `try_finalize_round` reads `tally()`
//! and resets the aggregator on a quorum round. Per-peer dedup defeats
//! single-peer vote flooding; per-(height, voter) supersession (below) keeps the
//! tally a single-round SNAPSHOT rather than a union over time (QC-004); and
//! k-sampling caps how many peers any hash can draw on per round. The historical
//! `ConsensusManager::record_response` per-hash counter with no peer identity is
//! now `#[cfg(test)]`-only.
//!
//! It is generic over the peer-key type `P` so the node instantiates it with the
//! authenticated `PeerId` it threads through at the intake site.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use commputer_core::block::BlockHash;
use rand::seq::SliceRandom;

/// Accumulates peer-keyed votes for one or more heights, deduping by peer and
/// k-sampling the deduped voter set when producing a round tally.
pub struct VoteAggregator<P> {
    /// k — the maximum number of distinct peers sampled into a single tally.
    /// Mirrors `SnowballParams::sample_size`.
    sample_size: usize,
    /// Deduped votes: `(height, block_hash)` -> the set of peers that voted for it.
    /// Using a set per key makes a repeat vote from the same peer a no-op, so no
    /// peer can inflate a hash's count beyond 1.
    votes: HashMap<(u64, BlockHash), HashSet<P>>,
    /// QC-004: each voter's CURRENT vote per height. A peer that later votes a
    /// different hash at the same height supersedes its earlier one — the earlier
    /// vote is removed from `votes` before the new one is recorded, so a voter is
    /// never counted for two hashes and the tally stays a single-round snapshot
    /// rather than a union over time.
    last_vote: HashMap<(u64, P), BlockHash>,
}

impl<P: Eq + Hash + Clone> VoteAggregator<P> {
    /// Create an aggregator that samples at most `sample_size` (k) peers per tally.
    pub fn new(sample_size: usize) -> Self {
        Self {
            sample_size,
            votes: HashMap::new(),
            last_vote: HashMap::new(),
        }
    }

    /// Update k when the network rescales (mirrors `SnowballParams::sample_size`).
    /// Recorded votes are kept — only the per-tally sampling bound changes.
    ///
    /// Called by `ConsensusManager::apply_rung` as the (1→20) peer curve grows:
    /// a stale small `k` (e.g. `k=1`) would cap every `tally()` below the new
    /// quorum and deadlock finalization mid-round.
    pub fn set_sample_size(&mut self, sample_size: usize) {
        self.sample_size = sample_size;
    }

    /// Record `peer`'s vote for `block_hash` at `height`.
    ///
    /// QC-004 supersession: if `peer` already voted a DIFFERENT hash at this
    /// height, that earlier vote is removed before the new one is recorded, so a
    /// voter is counted for at most one hash per height. Returns `true` if this
    /// vote was newly counted, `false` if `peer` had already voted for this same
    /// `(height, block_hash)` (deduped — a no-op).
    pub fn record_vote(&mut self, height: u64, block_hash: BlockHash, peer: P) -> bool {
        if let Some(prev) = self.last_vote.get(&(height, peer.clone())) {
            if *prev != block_hash {
                let prev_key = (height, *prev);
                if let Some(set) = self.votes.get_mut(&prev_key) {
                    set.remove(&peer);
                    if set.is_empty() {
                        self.votes.remove(&prev_key);
                    }
                }
            }
        }
        self.last_vote.insert((height, peer.clone()), block_hash);
        self.votes
            .entry((height, block_hash))
            .or_default()
            .insert(peer)
    }

    /// QC-004: the hash `peer` currently votes for at `height`, if any. Used to
    /// assert the single-round-snapshot invariant and to guard double-counting.
    pub fn has_voted(&self, height: u64, peer: &P) -> Option<BlockHash> {
        self.last_vote.get(&(height, peer.clone())).copied()
    }

    /// Deduped voter count for a single `(height, block_hash)`, ignoring sampling.
    /// This is the number of *distinct* peers that voted for the hash.
    pub fn deduped_count(&self, height: u64, block_hash: BlockHash) -> usize {
        self.votes
            .get(&(height, block_hash))
            .map(HashSet::len)
            .unwrap_or(0)
    }

    /// Number of distinct peers that voted for any hash at `height`.
    pub fn distinct_voters(&self, height: u64) -> usize {
        self.voter_set(height).len()
    }

    /// Produce a round tally for `height`: `block_hash` -> vote count, ready to
    /// feed to `SnowballVoter::record_round`.
    ///
    /// The deduped voter set for the height is sampled down to at most k peers;
    /// each hash is then counted only over the sampled peers. This bounds every
    /// hash's reported count by k (a peer flooding one hash still counts once,
    /// and the whole tally can draw on at most k distinct peers).
    pub fn tally(&self, height: u64, rng: &mut impl rand::Rng) -> HashMap<BlockHash, usize> {
        // The per-hash peer sets at this height.
        let slices: Vec<(&BlockHash, &HashSet<P>)> = self
            .votes
            .iter()
            .filter(|((h, _), _)| *h == height)
            .map(|((_, hash), peers)| (hash, peers))
            .collect();

        // Sample the deduped voter set down to k.
        let mut voters: Vec<&P> = self.voter_set(height).into_iter().collect();
        let k = self.sample_size.min(voters.len());
        voters.shuffle(rng);
        voters.truncate(k);
        let sampled: HashSet<&P> = voters.into_iter().collect();

        // Count each hash over the sampled peers only.
        let mut tally = HashMap::new();
        for (hash, peers) in slices {
            let count = peers.iter().filter(|p| sampled.contains(p)).count();
            if count > 0 {
                tally.insert(*hash, count);
            }
        }
        tally
    }

    /// The union of distinct peers that voted for any hash at `height`.
    fn voter_set(&self, height: u64) -> HashSet<&P> {
        self.votes
            .iter()
            .filter(|((h, _), _)| *h == height)
            .flat_map(|(_, peers)| peers.iter())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn hash(n: u8) -> BlockHash {
        let mut h = [0u8; 32];
        h[0] = n;
        BlockHash(h)
    }

    #[test]
    fn dedup_returns_false_on_repeat() {
        let mut agg: VoteAggregator<u64> = VoteAggregator::new(20);
        let h = hash(1);
        assert!(agg.record_vote(10, h, 7)); // first vote counts
        assert!(!agg.record_vote(10, h, 7)); // same peer, same hash → deduped
        assert_eq!(agg.deduped_count(10, h), 1);
    }

    #[test]
    fn single_peer_flood_counts_once_and_cannot_reach_quorum() {
        // One peer votes 100 times for the same hash. It must count as ONE, so
        // it can never on its own reach a quorum > 1.
        let mut agg: VoteAggregator<u64> = VoteAggregator::new(20);
        let h = hash(1);
        let flooder = 7u64;
        for _ in 0..100 {
            agg.record_vote(10, h, flooder);
        }
        assert_eq!(agg.deduped_count(10, h), 1);

        let mut rng = StdRng::seed_from_u64(1);
        let tally = agg.tally(10, &mut rng);
        assert_eq!(tally.get(&h), Some(&1));

        // A realistic quorum (e.g. 14) is unreachable from a single flooding peer.
        let quorum = 14usize;
        assert!(tally.get(&h).copied().unwrap_or(0) < quorum);
    }

    #[test]
    fn two_hashes_cross_quorum() {
        // Two competing hashes each backed by many distinct peers both survive
        // into the tally with their correct deduped counts.
        let mut agg: VoteAggregator<u64> = VoteAggregator::new(100);
        for p in 0..10u64 {
            agg.record_vote(5, hash(1), p);
        }
        for p in 100..104u64 {
            agg.record_vote(5, hash(2), p);
        }

        let mut rng = StdRng::seed_from_u64(2);
        let tally = agg.tally(5, &mut rng);
        // k=100 > 14 distinct voters, so all are sampled and counts are exact.
        assert_eq!(tally.get(&hash(1)), Some(&10));
        assert_eq!(tally.get(&hash(2)), Some(&4));
    }

    #[test]
    fn k_sampling_bounds_the_count() {
        // 50 distinct peers all vote for one hash; the tally is capped at k.
        let k = 5usize;
        let mut agg: VoteAggregator<u64> = VoteAggregator::new(k);
        for p in 0..50u64 {
            agg.record_vote(1, hash(1), p);
        }
        assert_eq!(agg.deduped_count(1, hash(1)), 50);
        assert_eq!(agg.distinct_voters(1), 50);

        let mut rng = StdRng::seed_from_u64(3);
        let tally = agg.tally(1, &mut rng);
        assert_eq!(tally.get(&hash(1)), Some(&k));
    }

    #[test]
    fn set_sample_size_rescales_the_tally_cap_on_existing_votes() {
        // Simulate the (1→20) network-size rescale: an aggregator starts with a
        // stale k=1, votes accumulate, then `update_params_for_network_size`
        // rescales k. The tally cap must track the NEW k over the votes already
        // recorded — otherwise the stale k=1 caps every tally below quorum and
        // finalization deadlocks.
        let mut agg: VoteAggregator<u64> = VoteAggregator::new(1);
        for p in 0..20u64 {
            agg.record_vote(3, hash(1), p);
        }
        assert_eq!(agg.deduped_count(3, hash(1)), 20);

        // Stale k=1: the tally is pinned at 1, well below a realistic quorum.
        let mut rng = StdRng::seed_from_u64(9);
        let stale = agg.tally(3, &mut rng);
        assert_eq!(stale.get(&hash(1)), Some(&1));

        // Rescale k to 20 (votes are kept). Now the same votes tally to 20,
        // clearing the quorum the stale k could never reach.
        agg.set_sample_size(20);
        assert_eq!(agg.deduped_count(3, hash(1)), 20); // votes untouched
        let rescaled = agg.tally(3, &mut rng);
        assert_eq!(rescaled.get(&hash(1)), Some(&20));
        let quorum = 14usize;
        assert!(rescaled.get(&hash(1)).copied().unwrap_or(0) >= quorum);
    }

    #[test]
    fn cross_time_vote_supersedes_not_unions() {
        // QC-004: a voter that later votes a different hash at the same height
        // supersedes its earlier vote — the tally is a single-round SNAPSHOT, not
        // a union over time. Without supersession P1 and P2 would each be counted
        // for BOTH X and Y, manufacturing a phantom X:2 AND Y:2 from two voters.
        let mut agg: VoteAggregator<u64> = VoteAggregator::new(20);
        let (x, y) = (hash(1), hash(2));
        agg.record_vote(5, x, 1);
        agg.record_vote(5, x, 2);
        assert_eq!(agg.deduped_count(5, x), 2);

        // Both voters drift to Y, with NO reset between (the sub-quorum window).
        agg.record_vote(5, y, 1);
        agg.record_vote(5, y, 2);

        // X is now empty (both superseded); only Y holds the two voters.
        assert_eq!(agg.deduped_count(5, x), 0, "superseded votes must not linger");
        assert_eq!(agg.deduped_count(5, y), 2);
        assert_eq!(agg.has_voted(5, &1), Some(y));
        assert_eq!(agg.has_voted(5, &2), Some(y));

        // The tally is a snapshot: Y:2 only, never X:2 AND Y:2.
        let mut rng = StdRng::seed_from_u64(1);
        let tally = agg.tally(5, &mut rng);
        assert_eq!(tally.get(&x), None);
        assert_eq!(tally.get(&y), Some(&2));
    }

    #[test]
    fn re_voting_the_same_hash_is_still_a_dedup_noop() {
        // Supersession must not break the existing dedup: a peer re-voting the
        // SAME hash stays counted once and is not spuriously removed.
        let mut agg: VoteAggregator<u64> = VoteAggregator::new(20);
        let x = hash(1);
        assert!(agg.record_vote(7, x, 1));
        assert!(!agg.record_vote(7, x, 1));
        assert_eq!(agg.deduped_count(7, x), 1);
        assert_eq!(agg.has_voted(7, &1), Some(x));
    }

    #[test]
    fn heights_are_isolated() {
        let mut agg: VoteAggregator<u64> = VoteAggregator::new(20);
        agg.record_vote(1, hash(1), 1);
        agg.record_vote(2, hash(1), 2);
        assert_eq!(agg.deduped_count(1, hash(1)), 1);
        assert_eq!(agg.deduped_count(2, hash(1)), 1);
        assert_eq!(agg.distinct_voters(1), 1);
    }
}
