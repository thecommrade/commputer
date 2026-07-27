#![allow(dead_code)]
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use serde::{Deserialize, Serialize};
use tracing::{info, debug, warn};

use commputer_core::block::{Block, BlockHash, BlockHeader};
use commputer_core::identity::Address;
use commputer_core::transaction::Transaction;
use commputer_consensus::snowball::{SnowballParams, SnowballVoter};
use commputer_consensus::VoteAggregator;
use libp2p::PeerId;

/// Result of a consensus finalization attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsensusRoundResult {
    /// Voting is still in progress, not yet converged.
    NotReady,
    /// Snowball voting converged -- block is finalized.
    Finalized,
    /// Consensus timed out without convergence -- node should consider resyncing.
    Stalled,
}

/// Consensus timeout scales with network size.
/// Small network (0-3 peers): 6s (3 block times)
/// Medium network (4-20): 10s
/// Large network (20+): 30s (original Avalanche assumption)
fn consensus_timeout_secs(peer_count: usize) -> u64 {
    match peer_count {
        0..=3 => 6,
        4..=20 => 10,
        _ => 30,
    }
}

/// Feature 126: Minimum block interval per validator (seconds).
pub const MIN_BLOCK_INTERVAL_SECS: u64 = 2;

/// Feature 139: Maximum allowed timestamp drift from network median (seconds).
pub const MAX_TIMESTAMP_DRIFT_SECS: u64 = 15;

/// Feature 121: View change protocol.
/// If the elected block producer is offline for this duration (seconds),
/// the next-highest CRS validator takes over block production.
pub const VIEW_CHANGE_TIMEOUT_SECS: u64 = 10;

/// Security bound: how far ahead of the last applied chain tip the consensus
/// manager will track heights. Remote peers can gossip `BlockCandidate` /
/// `BlockProposal` / `CheckpointCommitment` messages carrying an arbitrary,
/// unvalidated `height`. Without a window an attacker floods distinct
/// far-future heights and grows `heights` (plus each height's voter/candidate
/// structures), `height_start_time`, `validator_blocks`, and `checkpoint_votes`
/// without bound → remote OOM. The window is measured relative to `applied_tip`
/// (updated by `cleanup_below`), so legitimately-close-to-tip candidates are
/// never dropped — live consensus only ever votes at tip+1. Generous.
pub const MAX_HEIGHT_WINDOW: u64 = 1024;

/// Security bound: maximum number of distinct candidate blocks retained at a
/// single height. An attacker can mint many distinct block hashes at one height
/// by varying producer/timestamp; a real fork has at most one candidate per
/// validator, so this is far above any legitimate need.
pub const MAX_CANDIDATES_PER_HEIGHT: usize = 64;

/// Security bound: maximum number of distinct checkpoint validators tracked per
/// height. `CheckpointCommitment` carries an attacker-chosen `validator`
/// Address, so without a cap one peer synthesises unbounded addresses per
/// height. Far above any realistic validator-set size for an alpha testnet.
pub const MAX_CHECKPOINT_VALIDATORS_PER_HEIGHT: usize = 1024;

/// Per-peer budget for NotReady-driven stall-timer resets since the last
/// chain-tip advancement. See `ConsensusManager::allow_notready_stall_reset`.
pub const MAX_NOTREADY_STALL_RESETS: u32 = 5;

/// Security bound: maximum distinct peers tracked in the NotReady stall-reset
/// budget map. When full, an unseen peer is DENIED rather than evicting an
/// existing entry — eviction would let a rotating-PeerId attacker launder a
/// fresh budget through churn; denial only ever fails closed.
pub const MAX_NOTREADY_STALL_RESET_PEERS: usize = 64;

/// Feature 121: View change state — tracks when a view change is triggered.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ViewChange {
    /// The height at which the view change occurred.
    pub height: u64,
    /// The original producer who was expected to produce the block.
    pub original_producer: Address,
    /// The replacement producer who took over.
    pub replacement_producer: Address,
    /// When the view change was initiated.
    pub triggered_at: Instant,
    /// The view number (0 = original, 1 = first replacement, etc.).
    pub view_number: u32,
}

/// Messages exchanged between nodes for Snowball consensus and sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConsensusMessage {
    /// A block candidate for a given height.
    BlockCandidate {
        block: Block,
    },
    /// Query: "At this height, which block do you prefer?"
    /// `round` is a nonce that makes each query unique so gossipsub
    /// doesn't deduplicate repeated queries for the same height.
    SnowballQuery {
        height: u64,
        querier_preference: BlockHash,
        #[serde(default)]
        round: u64,
    },
    /// Response: "At this height, I prefer this block."
    /// `round` echoes the query's round nonce to prevent gossipsub dedup.
    SnowballResponse {
        height: u64,
        preference: BlockHash,
        #[serde(default)]
        round: u64,
    },
    /// Request a specific block by height (sync protocol).
    BlockRequest {
        height: u64,
    },
    /// Response to a block request.
    BlockResponse {
        block: Option<Block>,
        requested_height: u64,
    },

    /// Feature 247: Light client request — request merkle proof for a tx in a block.
    LightClientRequest {
        tx_hash: [u8; 32],
        block_height: u64,
    },

    /// Feature 247: Light client response — return the proof and block header.
    LightClientResponse {
        block_header: Option<BlockHeader>,
        merkle_proof: Option<commputer_core::merkle::MerkleProof>,
        tx_hash: [u8; 32],
    },

    /// Feature 133: Checkpoint commitment — validators sign a checkpoint.
    CheckpointCommitment {
        height: u64,
        state_root: [u8; 32],
        validator: Address,
    },

    /// Block proposal — carries the full block so peers can validate and vote
    /// in one round-trip. This IS the Snowball query on first broadcast.
    BlockProposal {
        block: Block,
        #[serde(default)]
        round: u64,
    },
    /// Lightweight retry query — references a block by height and preference hash.
    /// If the peer hasn't seen this block, it requests it via BlockRequest.
    BlockQuery {
        height: u64,
        preference: BlockHash,
        #[serde(default)]
        round: u64,
    },
    /// Vote response — peer's preference after evaluating a proposal or query.
    VoteResponse {
        height: u64,
        preference: BlockHash,
        #[serde(default)]
        round: u64,
    },
}

/// Per-height voting state: the voter plus all candidate blocks.
struct HeightState {
    voter: SnowballVoter,
    candidates: HashMap<BlockHash, Block>,
    /// Peer-keyed vote accumulator for the current round. Replaces the raw
    /// per-hash counter so a single peer counts at most once per
    /// (height, block_hash) -- this is the ONLY counting path, which keeps the
    /// Sybil-vulnerable increment unreachable. Reset each round in
    /// `try_finalize_round`; dies with its height via `take_finalized` /
    /// `cleanup_below` / `clear` (free lifecycle cleanup).
    aggregator: VoteAggregator<PeerId>,
    /// Whether the full block proposal has been sent at least once for this height.
    proposal_sent: bool,
    /// The candidate we have committed to at this height, fixed at our first
    /// vote and never changed afterwards.
    ///
    /// This is the classic BFT lock, and it is what makes a one-round decision
    /// (beta = 1, used at small validator counts) SAFE. Without it a node that
    /// answered with "lowest candidate hash" would switch its vote the moment a
    /// lower-hash candidate arrived, so the same node could contribute to two
    /// different majorities at one height and two conflicting blocks could each
    /// gather a quorum. Locking the choice means each node contributes to
    /// exactly one majority per height, and majorities intersect.
    locked_choice: Option<BlockHash>,
}

/// Manages Snowball consensus across active heights.
///
/// Lifecycle per height:
/// 1. `add_candidate()` registers a block and creates/updates the voter.
/// 2. `query_preference()` returns what to include in a SnowballQuery.
/// 3. `record_peer_response()` accumulates peer responses, keyed by the
///    authenticated PeerId so each peer counts once per round.
/// 4. `try_finalize_round()` feeds accumulated responses into the voter.
/// 5. `take_finalized()` returns the winning block and cleans up.
pub struct ConsensusManager {
    heights: HashMap<u64, HeightState>,
    params: SnowballParams,
    /// Feature 125: Track (validator, height) -> BlockHash to detect equivocation.
    pub validator_blocks: HashMap<(Address, u64), BlockHash>,
    /// Feature 125: Slashed validators for the current epoch — earn zero rewards.
    pub slashed_validators: HashSet<Address>,
    /// Feature 129: Track when each height started consensus.
    pub height_start_time: HashMap<u64, Instant>,
    /// Feature 121: View change history.
    pub view_changes: Vec<ViewChange>,
    /// Feature 126: Track last block timestamp per validator for rate limiting.
    pub last_block_time: HashMap<Address, u64>,
    /// Feature 133: Checkpoint commitments: height -> set of (validator, state_root).
    pub checkpoint_votes: HashMap<u64, HashMap<Address, [u8; 32]>>,
    /// Security: last applied chain tip, updated by `cleanup_below`. Bounds how
    /// far ahead of the tip candidate/checkpoint heights may be tracked so a
    /// remote peer cannot flood arbitrary heights and exhaust memory. Retained
    /// across `clear()` — a resync must not reopen the solo-bootstrap window.
    applied_tip: u64,
    /// Reserved for explicit ceremony bootstrap wiring: when true, the
    /// zero-peer solo-finalize gate in `try_finalize_round` is bypassed and a
    /// node may finalize with no peers at any height. Defaults to false;
    /// nothing sets it yet.
    pub allow_solo: bool,
    /// Per-peer count of stall-timer resets granted because that peer reported
    /// NotReady/Syncing (see `allow_notready_stall_reset`). Bounded at
    /// MAX_NOTREADY_STALL_RESET_PEERS; replenished on tip advancement in
    /// `cleanup_below` and on `clear`.
    notready_stall_resets: HashMap<PeerId, u32>,
}

impl ConsensusManager {
    /// Create a new consensus manager with default Snowball parameters.
    pub fn new() -> Self {
        Self {
            heights: HashMap::new(),
            params: SnowballParams {
                sample_size: 3,
                quorum: 2,
                decision_threshold: 5,
            },
            validator_blocks: HashMap::new(),
            slashed_validators: HashSet::new(),
            height_start_time: HashMap::new(),
            view_changes: Vec::new(),
            last_block_time: HashMap::new(),
            checkpoint_votes: HashMap::new(),
            applied_tip: 0,
            allow_solo: false,
            notready_stall_resets: HashMap::new(),
        }
    }

    /// Scale Snowball parameters to current network size.
    /// Stepped curve from solo-bootstrap (1,1,3) up to full production
    /// (20,14,20) at peer_count >= 21. Each rung satisfies the validate()
    /// invariant `quorum > sample_size / 2`. The (3,2,5) rung at
    /// peer_count in [3,5] preserves the bbbed4f-validated 3-node stress
    /// behaviour. See ADR-0002 (docs/adrs/0002_snowball_consensus.md) and
    /// `src/consensus/src/config.rs` for the parameter envelope.
    pub fn update_params_for_network_size(&mut self, peer_count: usize) {
        let (sample, quorum, threshold): (usize, usize, u32) = match peer_count {
            // β=3 at 0 peers: defense in depth behind the zero-peer gate in
            // try_finalize_round — a stale-params race must not be able to
            // one-shot finalize a private block.
            // Quorum is a MAJORITY OF THE VOTERS, and the voters are
            // peers + ourselves (try_finalize_round counts our own preference).
            // Sizing these against peers alone left every rung one vote short:
            // at one peer, quorum 1 meant a single peer reply finalized a block,
            // and live 2026-07-25 a node with one visible peer finalized its own
            // block 4 while the other two finalized a different one — a fork at
            // the fourth block of a fresh chain.
            // SMALL-n: DETERMINISTIC, not sampled (beta = 1).
            //
            // Snowball's guarantees are asymptotic in validator count — its
            // safety bound is exponential in the sample size k, and k <= n. At
            // n<=6 we broadcast to everyone and count everyone, so the "sample"
            // IS the network: we pay the full cost of an all-to-all BFT round
            // and get a probabilistic guarantee that means nothing at this
            // size. Worse, beta>1 requires that many CONSECUTIVE quorum rounds
            // and resets on any miss, so a single slow tick discards all
            // progress — the source of repeated live stalls.
            //
            // With a majority quorum, deterministic vote selection, and a
            // locked per-height choice, one quorum certificate is final. That
            // is ordinary BFT and it is strictly stronger here than repeating
            // a degenerate sample. Sampling resumes at 6+ peers, where it
            // begins to buy something.
            0 => (1, 1, 1),          // n=1: solo; the zero-peer gate blocks it anyway
            1 => (2, 2, 1),          // n=2: both must agree
            2 => (3, 2, 1),          // n=3: 2 of 3 — tolerates one node down
            3..=5 => (4, 3, 1),      // n=4..6: 3 is a majority at n=4
            6..=10 => (5, 4, 8),
            11..=20 => (10, 7, 14),
            _ => (20, 14, 20),
        };

        self.params.sample_size = sample;
        self.params.quorum = quorum;
        self.params.decision_threshold = threshold;

        // Propagate to all existing voters so they use current network params.
        // Without this, voters created at startup keep stale default params
        // (quorum=2) which prevents finalization with 1 vote (quorum_choice = None).
        for state in self.heights.values_mut() {
            state.voter.set_params(self.params.clone());
            // Keep each aggregator's k (sample bound) in step with the rescaled
            // network size. A stale small k caps every tally below the new
            // quorum (e.g. k=1 vs quorum=14 at the 21+ rung) and would deadlock
            // finalization mid-round. `sample` is the k just chosen above.
            state.aggregator.set_sample_size(sample);
        }
    }

    /// Create a consensus manager with custom Snowball parameters (Feature 131).
    pub fn with_params(params: SnowballParams) -> Self {
        Self {
            heights: HashMap::new(),
            params,
            validator_blocks: HashMap::new(),
            slashed_validators: HashSet::new(),
            height_start_time: HashMap::new(),
            view_changes: Vec::new(),
            last_block_time: HashMap::new(),
            checkpoint_votes: HashMap::new(),
            applied_tip: 0,
            allow_solo: false,
            notready_stall_resets: HashMap::new(),
        }
    }

    /// Add a candidate block. Creates the voter for this height if needed.
    /// If there is only one candidate, it is immediately finalized.
    /// Feature 125: Detects equivocation (same validator, same height, different hash).
    /// `from_network` should be true for blocks received from remote peers;
    /// local retries (same producer, rejected block) should pass false to
    /// avoid false-positive equivocation detection.
    pub fn add_candidate(&mut self, block: Block) {
        self.add_candidate_inner(block, true);
    }

    /// Add a locally produced candidate without triggering equivocation detection.
    pub fn add_local_candidate(&mut self, block: Block) {
        self.add_candidate_inner(block, false);
    }

    fn add_candidate_inner(&mut self, block: Block, from_network: bool) {
        let height = block.height();
        let hash = block.hash();
        let producer = block.header.producer;

        // Security bound (see MAX_HEIGHT_WINDOW): drop candidates whose height is
        // outside the active window around the applied tip. A remote peer can
        // gossip empty/unsigned blocks at arbitrary heights; without this, both
        // `heights` and the per-height `height_start_time` / `validator_blocks`
        // entries grow without bound → OOM. Heights at/below the applied tip are
        // already finalized (stale); heights far above it are never legitimately
        // voted on. Checked BEFORE any map insertion so nothing can grow.
        if height <= self.applied_tip
            || height > self.applied_tip.saturating_add(MAX_HEIGHT_WINDOW)
        {
            return;
        }

        // Security bound (see MAX_CANDIDATES_PER_HEIGHT): if this height already
        // holds the maximum number of distinct candidates and this block is not
        // one of them, drop it before it can grow validator_blocks / candidates.
        // A peer cannot mint unbounded hashes (varying producer/timestamp) at one
        // height; existing candidates are always preserved.
        if let Some(state) = self.heights.get(&height)
            && !state.candidates.contains_key(&hash)
            && state.candidates.len() >= MAX_CANDIDATES_PER_HEIGHT
        {
            return;
        }

        // Feature 125: Check for equivocation — only flag blocks received from the
        // network. Local blocks are tracked (for we_produced_at) but don't trigger slashing.
        let key = (producer, height);
        if let Some(existing_hash) = self.validator_blocks.get(&key) {
            if *existing_hash != hash && from_network {
                warn!(
                    "EQUIVOCATION DETECTED: validator {} signed two different blocks at height {} ({} and {})",
                    producer, height, existing_hash, hash
                );
                self.slashed_validators.insert(producer);
            }
        } else {
            self.validator_blocks.insert(key, hash);
        }

        // Feature 129: Track height start time.
        self.height_start_time.entry(height).or_insert_with(Instant::now);

        let state = self.heights.entry(height).or_insert_with(|| HeightState {
            voter: SnowballVoter::new(self.params.clone()),
            candidates: HashMap::new(),
            aggregator: VoteAggregator::new(self.params.sample_size),
            proposal_sent: false,
            locked_choice: None,
        });

        // Don't re-add duplicates.
        if state.candidates.contains_key(&hash) {
            return;
        }

        state.candidates.insert(hash, block);
        // Deterministic symmetric tie-break: converge every node's INITIAL
        // preference on the lowest candidate hash. A fully-meshed set of
        // competing producers otherwise deadlocks — each node prefers its own
        // (first-arrived) block and no (q,q,β) quorum ever forms (live
        // 2026-07-25: the all-alpha.5 triangle froze at height 31 with three
        // producers). Only the un-voted initial preference moves; once
        // sampling starts, preference changes belong to Snowball's quorum
        // dynamics alone.
        if let Some(lowest) = state.candidates.keys().min().copied() {
            state.voter.reset_initial_preference_if_unvoted(lowest);
        }
        debug!("Candidate added at height {}: {} (total: {})", height, hash, state.candidates.len());
    }

    /// Returns the voter's current preference at a given height, if any.
    pub fn query_preference(&self, height: u64) -> Option<BlockHash> {
        self.heights.get(&height).and_then(|s| s.voter.preference())
    }

    /// Diagnostic: (candidate hash, its parent hash) for every candidate at a
    /// height, so a refusal to vote can say WHY — which parent each candidate
    /// claims versus the tip the voter actually holds.
    pub fn candidate_parents(&self, height: u64) -> Vec<(BlockHash, BlockHash)> {
        self.heights
            .get(&height)
            .map(|s| {
                s.candidates
                    .iter()
                    .map(|(h, b)| (*h, b.header.parent_hash))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// VOTE-HEIGHT DISCIPLINE (alpha.6): the preference we may legitimately
    /// endorse at `height`, given our own applied tip hash — i.e. only a
    /// candidate that BUILDS ON THE CHAIN WE ACTUALLY HOLD.
    ///
    /// Previously a node answered accept-votes using `query_preference` at any
    /// height it had a candidate for, including heights above its applied tip.
    /// Those votes are unbacked: the voter has not applied the parent and
    /// cannot know the block is valid. A producer could therefore reach quorum
    /// on rubber-stamps and finalize far ahead of everyone who would have to
    /// apply the result — live 2026-07-25, one node finalized 107..123 alone
    /// while its two peers sat at 106.
    ///
    /// Returns the Snowball preference when that candidate extends our tip
    /// (so normal fork resolution is untouched), else the lowest-hash
    /// candidate that does extend it (matching the deterministic tie-break),
    /// else None — meaning "cannot endorse", which the caller answers with
    /// NotReady rather than a vote.
    pub fn query_votable_preference(
        &mut self,
        height: u64,
        tip_hash: BlockHash,
    ) -> Option<BlockHash> {
        let state = self.heights.get_mut(&height)?;
        // Locked: keep voting for what we already committed to, as long as it
        // still builds on our tip. Changing our vote mid-height would let one
        // node contribute to two different majorities (see `locked_choice`).
        if let Some(locked) = state.locked_choice
            && state.candidates.get(&locked).is_some_and(|b| b.header.parent_hash == tip_hash)
        {
            return Some(locked);
        }
        let state = self.heights.get(&height)?;
        // DETERMINISTIC: always the lowest-hash candidate extending our tip —
        // never "whichever we happened to prefer first".
        //
        // Answering with our own preference splits the vote when several
        // producers are live: each node endorses its own candidate, no hash
        // reaches quorum, and at small peer counts (where quorum equals the
        // number of voters) the chain halts outright. The initial-preference
        // tie-break does not save it, because that only re-points while no
        // votes have been recorded. Live 2026-07-26: two nodes at the same tip
        // each voted for their own block at height 897 and the chain stopped.
        //
        // A pure function of (candidate set, our tip) makes every honest node
        // answer identically, so the tally concentrates instead of splitting.
        let choice = state
            .candidates
            .iter()
            .filter(|(_, b)| b.header.parent_hash == tip_hash)
            .map(|(h, _)| *h)
            .min();
        // Commit to it: from here on this height, this is our vote.
        if let Some(c) = choice
            && let Some(st) = self.heights.get_mut(&height)
        {
            st.locked_choice = Some(c);
        }
        choice
    }

    /// Get the preferred candidate block at a height (for re-proposing).
    pub fn get_candidate_block(&self, height: u64) -> Option<Block> {
        let state = self.heights.get(&height)?;
        let pref = state.voter.preference()?;
        state.candidates.get(&pref).cloned()
    }

    /// Whether the full block proposal has been sent for this height.
    pub fn proposal_sent(&self, height: u64) -> bool {
        self.heights.get(&height).map(|s| s.proposal_sent).unwrap_or(false)
    }

    /// Mark that the full block proposal has been sent for this height.
    pub fn mark_proposal_sent(&mut self, height: u64) {
        if let Some(state) = self.heights.get_mut(&height) {
            state.proposal_sent = true;
        }
    }

    /// Record a peer's response for a given height, attributed to `peer`.
    /// Routes through the peer-keyed `VoteAggregator`: a repeat vote from the
    /// same peer for the same (height, preference) is a no-op that returns
    /// `false`, so single-peer flooding cannot fabricate a quorum. Returns
    /// `true` when the vote was newly counted this round.
    pub fn record_peer_response(&mut self, height: u64, preference: BlockHash, peer: PeerId) -> bool {
        if let Some(state) = self.heights.get_mut(&height)
            && !state.voter.is_finalized() {
                let newly = state.aggregator.record_vote(height, preference, peer);
                if !newly {
                    debug!(
                        "Deduped duplicate Snowball vote from {} at height {} for {}",
                        peer, height, preference
                    );
                }
                return newly;
            }
        false
    }

    /// Legacy count-based entry point, retained as a TEST-ONLY delegating shim.
    /// The protected batch switched every production feed site to
    /// `record_peer_response` (PeerId-dedup), so this Sybil-countable path is now
    /// `#[cfg(test)]` — gone from the production API, kept only so this module's
    /// tests exercise the tally with the exact pre-dedup semantics (each call is a
    /// fresh random PeerId ⇒ a distinct voter ⇒ N calls add N to the round tally).
    #[cfg(test)]
    pub fn record_response(&mut self, height: u64, preference: BlockHash) {
        self.record_peer_response(height, preference, PeerId::random());
    }

    /// Feed accumulated responses into the voter and reset for the next round.
    /// `peer_count` is used to scale the timeout to network size.
    /// Returns a `ConsensusRoundResult` indicating the outcome.
    pub fn try_finalize_round(&mut self, height: u64, peer_count: usize) -> ConsensusRoundResult {
        // Zero-peer solo-finalize gate: at 0 peers the solo rung would let a
        // node self-vote its own candidate to finalization every stall timeout,
        // minting a private fork (observed 4x live, 2026-07-24). Only the
        // genesis ceremony — block 1 on a virgin chain (applied_tip == 0) —
        // may finalize alone. Checked BEFORE the tally and BEFORE the timeout
        // so no Stalled can escape and, critically, the aggregator's pending
        // votes are NOT consumed: the round resumes when a peer reconnects.
        if peer_count == 0
            && !(height <= 1 && self.applied_tip == 0)
            && !self.allow_solo
        {
            return ConsensusRoundResult::NotReady;
        }

        if let Some(state) = self.heights.get_mut(&height) {
            if state.voter.is_finalized() {
                return ConsensusRoundResult::NotReady;
            }

            // Try to finalize from accumulated peer votes FIRST. The tally is
            // peer-deduped and k-sampled by the aggregator. A non-empty tally
            // CONSUMES the round: reset the aggregator to a fresh one (matching
            // the old `mem::take` on round_responses, so Snowball's
            // beta-consecutive-round semantics are preserved) before feeding the
            // round into the voter.
            let mut tally = state.aggregator.tally(height, &mut rand::thread_rng());
            if !tally.is_empty() {
                // Count OUR OWN vote. The aggregator only holds peer responses,
                // so a quorum of q previously meant "q PEERS agree" — with two
                // peers and quorum 2 that is unanimity, and any single peer
                // being slow, restarting, or refusing freezes every round. We
                // are a validator too and our preference IS our vote, so
                // counting it makes quorum q mean "q of the n=peers+1 voters",
                // i.e. a real majority: 2-of-3 tolerates one node down.
                //
                // Added INSIDE the non-empty branch on purpose: a round is
                // still driven by peer responses. Adding it unconditionally
                // would make every tick a round, and a round that misses quorum
                // resets Snowball's consecutive-round counter — which would
                // stop confidence ever reaching beta.
                // Our self-vote must use the SAME deterministic rule we answer
                // peers with (lowest candidate hash), not the voter's internal
                // preference. The internal preference is whichever candidate
                // arrived first, so it can permanently disagree with what every
                // peer is voting for — and where quorum equals the voter count,
                // one dissenting self-vote deadlocks the height forever. Live
                // 2026-07-26: 435 votes arrived at height 897 and nothing ever
                // finalized.
                // Our locked choice if we have one, else the deterministic
                // lowest hash. Same rule as the vote we send peers.
                if let Some(ours) = state
                    .locked_choice
                    .or_else(|| state.candidates.keys().min().copied())
                {
                    *tally.entry(ours).or_insert(0) += 1;
                }
                state.aggregator = VoteAggregator::new(self.params.sample_size);
                let finalized = state.voter.record_round(&tally);
                if finalized {
                    info!(
                        "Snowball finalized at height {}: {:?}",
                        height,
                        state.voter.finalized_hash()
                    );
                    return ConsensusRoundResult::Finalized;
                }
            }

            // Timeout detection -- signal stall instead of fabricating votes.
            let timeout = consensus_timeout_secs(peer_count);
            if let Some(start) = self.height_start_time.get(&height)
                && start.elapsed().as_secs() >= timeout {
                    warn!("Consensus stalled at height {} (timeout {}s, {} peers)",
                        height, timeout, peer_count);
                    return ConsensusRoundResult::Stalled;
                }

            ConsensusRoundResult::NotReady
        } else {
            ConsensusRoundResult::NotReady
        }
    }

    /// Feature 125: Check if a validator has been slashed for equivocation.
    pub fn is_slashed(&self, addr: &Address) -> bool {
        self.slashed_validators.contains(addr)
    }

    /// Feature 125: Reset slashing state at epoch boundary.
    /// Only clears slashed set. validator_blocks is retained for in-flight
    /// heights to prevent cross-epoch equivocation. cleanup_below handles
    /// pruning old entries after finalization.
    pub fn reset_epoch_slashing(&mut self) {
        self.slashed_validators.clear();
    }

    /// If the vote at `height` is finalized, return the winning block hash.
    pub fn finalized_at_height(&self, height: u64) -> Option<BlockHash> {
        self.heights
            .get(&height)
            .and_then(|s| s.voter.finalized_hash())
    }

    /// Take the finalized block out of the manager, cleaning up that height's state.
    /// Returns None if not yet finalized or height unknown.
    ///
    /// If the winning hash's block BODY is not among our candidates (a quorum
    /// can form around a hash we only ever saw in votes), the height state is
    /// left INTACT and None is returned — removing it would destroy the round
    /// and strand the quorum, locking the producer out until resync (free
    /// producer-lockout; alpha.6 panel finding). The body arrives later via
    /// gossip/sync and a subsequent take succeeds.
    pub fn take_finalized(&mut self, height: u64) -> Option<Block> {
        self.take_finalized_with_lost(height).map(|(block, _)| block)
    }

    /// Like `take_finalized`, but also returns every transaction that was
    /// packed into a LOSING candidate at this height.
    ///
    /// A proposer moves txs OUT of its mempool and INTO its candidate when it
    /// produces (`std::mem::take` in block production). When that candidate
    /// loses the round, those txs exist nowhere except the dropped block —
    /// so without this, any tx that rode a losing proposal is silently
    /// destroyed, network-wide once every pool's copy has lost a race.
    /// (Live finding: every faucet dispense died this way within ~3s — the
    /// WAN-lagged seed loses essentially every proposal race to the LAN pair,
    /// which lock each other's candidates first.)
    ///
    /// Losers from ALL producers are returned, not just our own: every node
    /// that saw a losing proposal can resurrect its txs, so inclusion no
    /// longer depends on the losing proposer ever winning a round. The caller
    /// requeues them (minus any the winner already included); duplicates and
    /// stale copies die in the producer's nonce filter or the post-apply
    /// mempool prune.
    pub fn take_finalized_with_lost(
        &mut self,
        height: u64,
    ) -> Option<(Block, Vec<Transaction>)> {
        let hash = self.finalized_at_height(height)?;
        if !self
            .heights
            .get(&height)
            .is_some_and(|s| s.candidates.contains_key(&hash))
        {
            return None;
        }
        let state = self.heights.remove(&height)?;
        let mut winner = None;
        let mut lost_txs = Vec::new();
        for block in state.candidates.into_values() {
            if block.hash() == hash {
                winner = Some(block);
            } else {
                lost_txs.extend(block.transactions);
            }
        }
        winner.map(|block| (block, lost_txs))
    }

    /// Surrender the txs of every candidate at `height` that is NOT the
    /// applied block — the SYNC-apply twin of `take_finalized_with_lost`.
    /// A height applied via sync never goes through `take_finalized` (we were
    /// behind; the round may not even be finalized locally), so `cleanup_below`
    /// would destroy the losing candidates and every tx inside them — exactly
    /// the loss path a WAN-lagged node hits most, since its recovery IS sync.
    /// Consumes the height's round state, matching take semantics.
    pub fn surrender_lost_at(&mut self, height: u64, applied: &BlockHash) -> Vec<Transaction> {
        let Some(state) = self.heights.remove(&height) else {
            return Vec::new();
        };
        let mut lost_txs = Vec::new();
        for block in state.candidates.into_values() {
            if block.hash() != *applied {
                lost_txs.extend(block.transactions);
            }
        }
        lost_txs
    }

    /// Remove consensus state for heights at or below the applied chain tip.
    /// Prevents memory leaks from stale entries that were finalized but never taken,
    /// or heights that timed out and were superseded.
    pub fn cleanup_below(&mut self, applied_height: u64) {
        self.heights.retain(|h, _| *h > applied_height);
        self.height_start_time.retain(|h, _| *h > applied_height);
        self.validator_blocks.retain(|(_, h), _| *h > applied_height);
        // Security: also prune checkpoint votes for finalized heights. Without
        // this the attacker-keyed (height, validator) map is never bounded and
        // grows without limit → OOM.
        self.checkpoint_votes.retain(|h, _| *h > applied_height);
        // Tip advancement means the chain is making progress again — replenish
        // every peer's NotReady stall-reset budget (see
        // allow_notready_stall_reset). Must run before the tip update below.
        if applied_height > self.applied_tip {
            self.notready_stall_resets.clear();
        }
        // Security: record the applied tip so add_candidate / record_checkpoint_vote
        // can bound how far ahead of it they will track heights. Monotonic — the
        // chain tip only advances.
        self.applied_tip = self.applied_tip.max(applied_height);
    }

    /// Clear all consensus state. Used during chain resync.
    /// `applied_tip` is deliberately retained: a resync on an established
    /// chain must not reopen the zero-peer solo-bootstrap window.
    pub fn clear(&mut self) {
        self.heights.clear();
        self.height_start_time.clear();
        self.validator_blocks.clear();
        self.slashed_validators.clear();
        self.view_changes.clear();
        self.last_block_time.clear();
        self.checkpoint_votes.clear();
        self.notready_stall_resets.clear();
    }

    /// Budgeted permission for a peer's NotReady/Syncing status to reset the
    /// local stall timer. Bounds how long a permanently-Syncing (or hostile)
    /// peer can suppress a neighbor's stall recovery: each distinct peer may
    /// cause at most MAX_NOTREADY_STALL_RESETS resets since the last chain-tip
    /// advancement. Per-peer so one hostile peer cannot drain the shield that
    /// protects honest syncing peers. Returns true (and spends one unit of the
    /// peer's budget) while under budget; false once exhausted, or when the
    /// tracking map is full and the peer is unseen (deny, never evict — the
    /// safe direction). Budgets replenish in `cleanup_below` on tip
    /// advancement and in `clear`.
    pub fn allow_notready_stall_reset(&mut self, peer: PeerId) -> bool {
        if let Some(count) = self.notready_stall_resets.get_mut(&peer) {
            if *count >= MAX_NOTREADY_STALL_RESETS {
                return false;
            }
            *count += 1;
            return true;
        }
        if self.notready_stall_resets.len() >= MAX_NOTREADY_STALL_RESET_PEERS {
            return false;
        }
        self.notready_stall_resets.insert(peer, 1);
        true
    }

    /// How many candidates exist at a given height.
    pub fn candidates_at_height(&self, height: u64) -> usize {
        self.heights
            .get(&height)
            .map(|s| s.candidates.len())
            .unwrap_or(0)
    }

    /// All heights that currently have an active (non-finalized) vote.
    pub fn active_heights(&self) -> Vec<u64> {
        self.heights
            .iter()
            .filter(|(_, s)| !s.voter.is_finalized())
            .map(|(&h, _)| h)
            .collect()
    }

    /// All heights that have a finalized winner ready to be applied.
    pub fn finalized_heights(&self) -> Vec<u64> {
        self.heights
            .iter()
            .filter(|(_, s)| s.voter.is_finalized())
            .map(|(&h, _)| h)
            .collect()
    }

    /// Whether there is an active (non-finalized) vote at a given height.
    pub fn has_active_vote(&self, height: u64) -> bool {
        self.heights
            .get(&height)
            .map(|s| !s.voter.is_finalized())
            .unwrap_or(false)
    }

    /// Whether we know about any activity at the given height.
    pub fn has_height(&self, height: u64) -> bool {
        self.heights.contains_key(&height)
    }

    /// Whether the given validator already produced a block at this height.
    /// Used to prevent equivocation (producing two different blocks at the same height).
    pub fn we_produced_at(&self, height: u64, our_addr: &Address) -> bool {
        self.validator_blocks.contains_key(&(*our_addr, height))
    }

    /// Feature 121: Record a view change event.
    pub fn record_view_change(
        &mut self,
        height: u64,
        original_producer: Address,
        replacement_producer: Address,
        view_number: u32,
    ) {
        let vc = ViewChange {
            height,
            original_producer,
            replacement_producer,
            triggered_at: Instant::now(),
            view_number,
        };
        warn!(
            "View change at height {}: {} -> {} (view #{})",
            height, original_producer, replacement_producer, view_number
        );
        self.view_changes.push(vc);
    }

    /// Feature 121: Check if a view change should be triggered for the given height.
    /// Returns true if no block has been seen for VIEW_CHANGE_TIMEOUT_SECS.
    pub fn should_view_change(&self, height: u64) -> bool {
        if let Some(start) = self.height_start_time.get(&height) {
            start.elapsed().as_secs() >= VIEW_CHANGE_TIMEOUT_SECS
        } else {
            false
        }
    }

    /// Feature 126: Check if a block from this validator is rate-limited.
    /// Returns true if the block is too fast (less than MIN_BLOCK_INTERVAL_SECS
    /// since the last block from this validator).
    pub fn is_block_rate_limited(&self, producer: &Address, timestamp: u64) -> bool {
        if let Some(&last_time) = self.last_block_time.get(producer) {
            timestamp < last_time + MIN_BLOCK_INTERVAL_SECS
        } else {
            false
        }
    }

    /// Feature 126: Record when a validator produced a block.
    pub fn record_block_time(&mut self, producer: Address, timestamp: u64) {
        self.last_block_time.insert(producer, timestamp);
    }

    /// Feature 127: Check if a block should be suppressed (empty block with no
    /// transactions and no proof summaries).
    pub fn should_suppress_empty_block(block: &Block) -> bool {
        block.transactions.is_empty()
            && block.proof_summaries.is_empty()
            && block.epoch_summary.is_none()
    }

    /// Feature 132: Batch-finalize multiple blocks at once when catching up.
    /// Takes blocks sorted by height and finalizes them directly without
    /// running the full Snowball protocol (used during sync/catchup).
    pub fn batch_finalize_catchup(&mut self, blocks: Vec<Block>) -> Vec<Block> {
        let mut finalized = Vec::new();
        for block in blocks {
            let height = block.height();
            let hash = block.hash();

            // Track validator block for equivocation detection.
            let key = (block.header.producer, height);
            self.validator_blocks.entry(key).or_insert(hash);

            finalized.push(block);
        }
        finalized
    }

    /// Feature 133: Record a checkpoint commitment from a validator.
    pub fn record_checkpoint_vote(
        &mut self,
        height: u64,
        validator: Address,
        state_root: [u8; 32],
    ) {
        // Security bound (see MAX_HEIGHT_WINDOW): ignore checkpoint votes whose
        // height is far from the applied tip in either direction. Combined with
        // the cleanup_below prune this bounds the number of tracked heights;
        // legitimate recent-past and near-future checkpoints are always in range.
        if height > self.applied_tip.saturating_add(MAX_HEIGHT_WINDOW)
            || self.applied_tip > height.saturating_add(MAX_HEIGHT_WINDOW)
        {
            return;
        }
        let votes = self.checkpoint_votes.entry(height).or_default();
        // Security bound (see MAX_CHECKPOINT_VALIDATORS_PER_HEIGHT): cap distinct
        // validators per height so a peer cannot synthesise unbounded Addresses
        // to grow this height's vote set. Updates from already-seen validators
        // are always accepted.
        if !votes.contains_key(&validator)
            && votes.len() >= MAX_CHECKPOINT_VALIDATORS_PER_HEIGHT
        {
            return;
        }
        votes.insert(validator, state_root);
    }

    /// Feature 133: Check if a checkpoint has reached consensus (2/3+ agreement).
    pub fn checkpoint_consensus(
        &self,
        height: u64,
        total_validators: usize,
    ) -> Option<[u8; 32]> {
        if total_validators == 0 {
            return None;
        }
        let threshold = (total_validators * 2) / 3 + 1;

        if let Some(votes) = self.checkpoint_votes.get(&height) {
            // Count votes per state root.
            let mut root_counts: HashMap<[u8; 32], usize> = HashMap::new();
            for root in votes.values() {
                *root_counts.entry(*root).or_insert(0) += 1;
            }
            // Find any root with enough votes.
            for (root, count) in &root_counts {
                if *count >= threshold {
                    return Some(*root);
                }
            }
        }
        None
    }

    /// Feature 139: Validate a block's timestamp against the network median.
    /// Returns true if the timestamp is within acceptable drift.
    pub fn validate_timestamp(
        &self,
        block_timestamp: u64,
        recent_timestamps: &[u64],
    ) -> bool {
        if recent_timestamps.is_empty() {
            return true; // No reference — accept.
        }

        let mut sorted = recent_timestamps.to_vec();
        sorted.sort();
        let median = sorted[sorted.len() / 2];

        // Block timestamp must not be too far in the future or past.
        let drift = if block_timestamp > median {
            block_timestamp - median
        } else {
            median - block_timestamp
        };

        drift <= MAX_TIMESTAMP_DRIFT_SECS
    }
}

/// Feature 134: Generate a light client merkle proof for a transaction in a block.
/// Given a block and a transaction hash, returns the merkle proof and the index.
pub fn generate_light_client_proof(
    block: &Block,
    tx_hash: [u8; 32],
) -> Option<(commputer_core::merkle::MerkleProof, usize)> {
    let tx_hashes: Vec<[u8; 32]> = block.transactions.iter()
        .map(|tx| tx.hash().0)
        .collect();

    // Find the index of the transaction.
    let tx_index = tx_hashes.iter().position(|h| *h == tx_hash)?;

    let proof = commputer_core::merkle::generate_merkle_proof(&tx_hashes, tx_index)?;
    Some((proof, tx_index))
}

/// Feature 135: Verify a light client merkle proof without downloading the full block.
/// Given a tx hash, proof, and the block header's tx_root, verify inclusion.
pub fn verify_light_client_proof(
    tx_hash: [u8; 32],
    proof: &commputer_core::merkle::MerkleProof,
    tx_root: [u8; 32],
) -> bool {
    commputer_core::merkle::verify_merkle_proof(tx_hash, proof, tx_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use commputer_core::block::{Block, BlockHeader, BlockHash};
    use commputer_core::identity::Address;

    fn addr(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    fn make_test_block(height: u64) -> Block {
        make_test_block_with_producer(height, addr(0))
    }

    fn make_test_block_with_producer(height: u64, producer: Address) -> Block {
        make_test_block_with_timestamp(height, producer, 1000 + height)
    }

    fn make_test_block_with_timestamp(height: u64, producer: Address, timestamp: u64) -> Block {
        Block {
            header: BlockHeader {
                protocol_version: 1,
                height,
                parent_hash: BlockHash::GENESIS,
                tx_root: [0u8; 32],
                proof_root: [0u8; 32],
                state_root: [0u8; 32],
                timestamp,
                producer,
                epoch: 0,
                producer_public_key: vec![],
                signature: vec![],
                checkpoint_hash: None,
                chain_id: String::new(),
            },
            transactions: vec![],
            proof_summaries: vec![],
            compliance_summary: None, epoch_summary: None,
        }
    }

    // ---- Existing tests ----

    #[test]
    fn single_candidate_requires_voting() {
        let mut cm = ConsensusManager::new();
        let block = make_test_block(1);
        let hash = block.hash();
        cm.add_candidate(block);
        // Not finalized without peer responses, even with one candidate.
        assert_eq!(cm.finalized_at_height(1), None);
        cm.try_finalize_round(1, 3);
        // Still not finalized — no peer votes received.
        assert_eq!(cm.finalized_at_height(1), None);
        // Finalize via peer voting: 5 rounds (decision_threshold), 2 votes each (quorum).
        for _ in 0..5 {
            cm.record_response(1, hash);
            cm.record_response(1, hash);
            cm.try_finalize_round(1, 3);
        }
        assert_eq!(cm.finalized_at_height(1), Some(hash));
    }

    #[test]
    fn multiple_candidates_require_voting() {
        let mut cm = ConsensusManager::new();
        let block_a = make_test_block_with_producer(1, addr(1));
        let block_b = make_test_block_with_producer(1, addr(2));
        cm.add_candidate(block_a.clone());
        cm.add_candidate(block_b);
        // Not yet finalized — needs Snowball rounds.
        assert_eq!(cm.finalized_at_height(1), None);
        assert_eq!(cm.candidates_at_height(1), 2);
    }

    #[test]
    fn voting_converges_with_responses() {
        let mut cm = ConsensusManager::new();
        let block_a = make_test_block_with_producer(1, addr(1));
        let block_b = make_test_block_with_producer(1, addr(2));
        let hash_a = block_a.hash();
        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        // Simulate 5 rounds where all peers prefer block A.
        // With quorum=2, decision_threshold=5, this should finalize.
        for _ in 0..5 {
            cm.record_response(1, hash_a);
            cm.record_response(1, hash_a);
            cm.try_finalize_round(1, 3);
        }
        assert_eq!(cm.finalized_at_height(1), Some(hash_a));
    }

    #[test]
    fn take_finalized_removes_height() {
        let mut cm = ConsensusManager::new();
        let block = make_test_block(1);
        let hash = block.hash();
        cm.add_candidate(block);
        // Finalize via voting (no fast-path).
        for _ in 0..5 {
            cm.record_response(1, hash);
            cm.record_response(1, hash);
            cm.try_finalize_round(1, 3);
        }

        let taken = cm.take_finalized(1);
        assert!(taken.is_some());
        assert_eq!(taken.unwrap().hash(), hash);
        // Height should be gone now.
        assert_eq!(cm.finalized_at_height(1), None);
        assert!(!cm.has_height(1));
    }

    /// Simulate two consensus managers (two nodes) that both see two competing
    /// blocks at the same height. They exchange Snowball votes and must converge
    /// on the same winner.
    #[test]
    fn fork_resolution_two_managers_converge() {
        let mut cm_a = ConsensusManager::new();
        let mut cm_b = ConsensusManager::new();

        let block_1 = make_test_block_with_producer(1, addr(1));
        let block_2 = make_test_block_with_producer(1, addr(2));
        let hash_1 = block_1.hash();
        let hash_2 = block_2.hash();

        // Both nodes see both candidates (as would happen via gossipsub).
        cm_a.add_candidate(block_1.clone());
        cm_a.add_candidate(block_2.clone());
        cm_b.add_candidate(block_1);
        cm_b.add_candidate(block_2);

        // Neither should be finalized yet.
        assert_eq!(cm_a.finalized_at_height(1), None);
        assert_eq!(cm_b.finalized_at_height(1), None);

        // Simulate Snowball voting rounds. In each round:
        // - Each node queries its preference
        // - Both preferences are recorded as responses on the other node
        // - Both nodes try to finalize
        // We simulate a network where hash_1 has a slight majority.
        for round in 0..10 {
            let _pref_a = cm_a.query_preference(1);
            let _pref_b = cm_b.query_preference(1);

            // Simulate 3 peers voting (sample_size=3):
            // - 2 peers prefer hash_1, 1 prefers hash_2 (gives hash_1 the edge)
            let majority_hash = hash_1;
            let minority_hash = hash_2;

            // Feed majority preference to both managers.
            cm_a.record_response(1, majority_hash);
            cm_a.record_response(1, majority_hash);
            cm_a.record_response(1, minority_hash);

            cm_b.record_response(1, majority_hash);
            cm_b.record_response(1, majority_hash);
            cm_b.record_response(1, minority_hash);

            cm_a.try_finalize_round(1, 3);
            cm_b.try_finalize_round(1, 3);

            // Check if both finalized.
            let final_a = cm_a.finalized_at_height(1);
            let final_b = cm_b.finalized_at_height(1);

            if final_a.is_some() && final_b.is_some() {
                assert_eq!(
                    final_a, final_b,
                    "Both nodes must converge on the same block (round {})",
                    round
                );
                assert_eq!(
                    final_a.unwrap(), majority_hash,
                    "Winner should be the majority-preferred block"
                );
                eprintln!("Fork resolved in {} rounds — both chose {}", round + 1, majority_hash);
                return;
            }
        }

        // If we get here, both should at least have finalized by now with consistent voting.
        let final_a = cm_a.finalized_at_height(1);
        let final_b = cm_b.finalized_at_height(1);
        assert!(final_a.is_some(), "Node A should have finalized after 10 rounds");
        assert!(final_b.is_some(), "Node B should have finalized after 10 rounds");
        assert_eq!(final_a, final_b, "Both nodes must agree on the winner");
    }

    /// Test that fork resolution works even when the minority block initially
    /// gets some support before the majority prevails.
    #[test]
    fn fork_resolution_minority_loses_after_initial_support() {
        let mut cm = ConsensusManager::new();
        let block_a = make_test_block_with_producer(1, addr(10));
        let block_b = make_test_block_with_producer(1, addr(20));
        let hash_a = block_a.hash();
        let hash_b = block_b.hash();

        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        // First 2 rounds: block_b has majority (shouldn't finalize yet, need 5 consecutive).
        for _ in 0..2 {
            cm.record_response(1, hash_b);
            cm.record_response(1, hash_b);
            cm.record_response(1, hash_a);
            cm.try_finalize_round(1, 3);
        }
        assert_eq!(cm.finalized_at_height(1), None, "Should not finalize after only 2 rounds");

        // Next 5+ rounds: block_a takes over with strong majority.
        for _ in 0..6 {
            cm.record_response(1, hash_a);
            cm.record_response(1, hash_a);
            cm.record_response(1, hash_a);
            cm.try_finalize_round(1, 3);
        }

        // block_a should win since it had strong majority in later rounds.
        let winner = cm.finalized_at_height(1);
        assert!(winner.is_some(), "Should finalize after sufficient consistent rounds");
        assert_eq!(winner.unwrap(), hash_a, "Block A should win with sustained majority");
    }

    #[test]
    fn no_finalization_without_enough_rounds() {
        let mut cm = ConsensusManager::new();
        let block_a = make_test_block_with_producer(1, addr(1));
        let block_b = make_test_block_with_producer(1, addr(2));
        let hash_a = block_a.hash();
        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        // Only 3 rounds (need 5 for decision_threshold).
        for _ in 0..3 {
            cm.record_response(1, hash_a);
            cm.record_response(1, hash_a);
            cm.try_finalize_round(1, 3);
        }
        assert_eq!(cm.finalized_at_height(1), None);
    }

    // ---- Feature 121: View change tests ----

    #[test]
    fn view_change_recorded() {
        let mut cm = ConsensusManager::new();
        cm.record_view_change(10, addr(1), addr(2), 1);
        assert_eq!(cm.view_changes.len(), 1);
        assert_eq!(cm.view_changes[0].height, 10);
        assert_eq!(cm.view_changes[0].original_producer, addr(1));
        assert_eq!(cm.view_changes[0].replacement_producer, addr(2));
        assert_eq!(cm.view_changes[0].view_number, 1);
    }

    // ---- Feature 122: Extended equivocation detection tests ----

    #[test]
    fn equivocation_detected_and_slashed() {
        let mut cm = ConsensusManager::new();
        let validator = addr(1);

        // First block from validator at height 5.
        let block_a = make_test_block_with_producer(5, validator);
        cm.add_candidate(block_a);
        assert!(!cm.is_slashed(&validator));

        // Second different block from same validator at same height.
        let mut block_b = make_test_block_with_timestamp(5, validator, 1006);
        // Give it a different state root to make hash differ.
        block_b.header.state_root = [1u8; 32];
        cm.add_candidate(block_b);

        // Should now be slashed.
        assert!(cm.is_slashed(&validator));
    }

    #[test]
    fn same_block_twice_not_equivocation() {
        let mut cm = ConsensusManager::new();
        let validator = addr(1);

        let block = make_test_block_with_producer(5, validator);
        let block_clone = block.clone();
        cm.add_candidate(block);
        cm.add_candidate(block_clone); // Same block — not equivocation.

        assert!(!cm.is_slashed(&validator));
    }

    #[test]
    fn different_heights_not_equivocation() {
        let mut cm = ConsensusManager::new();
        let validator = addr(1);

        let block_a = make_test_block_with_producer(5, validator);
        let block_b = make_test_block_with_producer(6, validator);
        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        // Different heights — this is normal behavior, not equivocation.
        assert!(!cm.is_slashed(&validator));
    }

    #[test]
    fn different_validators_same_height_not_equivocation() {
        let mut cm = ConsensusManager::new();

        let block_a = make_test_block_with_producer(5, addr(1));
        let block_b = make_test_block_with_producer(5, addr(2));
        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        assert!(!cm.is_slashed(&addr(1)));
        assert!(!cm.is_slashed(&addr(2)));
    }

    #[test]
    fn multiple_equivocators_all_slashed() {
        let mut cm = ConsensusManager::new();

        // Validator 1 equivocates.
        let b1a = make_test_block_with_producer(5, addr(1));
        let mut b1b = make_test_block_with_timestamp(5, addr(1), 1006);
        b1b.header.state_root = [1u8; 32];
        cm.add_candidate(b1a);
        cm.add_candidate(b1b);

        // Validator 2 equivocates.
        let b2a = make_test_block_with_producer(5, addr(2));
        let mut b2b = make_test_block_with_timestamp(5, addr(2), 1007);
        b2b.header.state_root = [2u8; 32];
        cm.add_candidate(b2a);
        cm.add_candidate(b2b);

        assert!(cm.is_slashed(&addr(1)));
        assert!(cm.is_slashed(&addr(2)));
    }

    // ---- Feature 123: Slashing ensures zero epoch rewards ----

    #[test]
    fn slashed_validator_gets_zero_rewards() {
        let mut cm = ConsensusManager::new();
        let validator = addr(1);

        // Cause equivocation.
        let b1 = make_test_block_with_producer(5, validator);
        let mut b2 = make_test_block_with_timestamp(5, validator, 1006);
        b2.header.state_root = [1u8; 32];
        cm.add_candidate(b1);
        cm.add_candidate(b2);

        // Validator is slashed — should get zero epoch rewards.
        assert!(cm.is_slashed(&validator));

        // Simulate reward calculation: slashed validators get 0.
        let base_reward = 1000u64;
        let actual_reward = if cm.is_slashed(&validator) { 0 } else { base_reward };
        assert_eq!(actual_reward, 0);

        // Non-slashed validator gets full reward.
        let non_slashed = addr(2);
        let non_slashed_reward = if cm.is_slashed(&non_slashed) { 0 } else { base_reward };
        assert_eq!(non_slashed_reward, 1000);
    }

    #[test]
    fn slashing_resets_at_epoch_boundary() {
        let mut cm = ConsensusManager::new();
        let validator = addr(1);

        // Cause slashing.
        let b1 = make_test_block_with_producer(5, validator);
        let mut b2 = make_test_block_with_timestamp(5, validator, 1006);
        b2.header.state_root = [1u8; 32];
        cm.add_candidate(b1);
        cm.add_candidate(b2);
        assert!(cm.is_slashed(&validator));

        // Reset at epoch boundary.
        cm.reset_epoch_slashing();
        assert!(!cm.is_slashed(&validator));
    }

    // ---- Feature 126: Block production rate limiting ----

    #[test]
    fn block_rate_limiting() {
        let mut cm = ConsensusManager::new();
        let validator = addr(1);

        cm.record_block_time(validator, 1000);

        // Block at 1001 (only 1s later) should be rate-limited.
        assert!(cm.is_block_rate_limited(&validator, 1001));

        // Block at 1002 (exactly 2s later) should be allowed.
        assert!(!cm.is_block_rate_limited(&validator, 1002));

        // Block at 1005 (5s later) should be allowed.
        assert!(!cm.is_block_rate_limited(&validator, 1005));
    }

    #[test]
    fn block_rate_limiting_unknown_validator() {
        let cm = ConsensusManager::new();
        // Unknown validator — no rate limit.
        assert!(!cm.is_block_rate_limited(&addr(99), 1000));
    }

    // ---- Feature 127: Empty block suppression ----

    #[test]
    fn empty_block_suppressed() {
        let block = make_test_block(1);
        assert!(ConsensusManager::should_suppress_empty_block(&block));
    }

    #[test]
    fn block_with_transactions_not_suppressed() {
        use commputer_core::transaction::{Transaction, TxKind};
        use commputer_core::token::Amount;

        let mut block = make_test_block(1);
        block.transactions.push(Transaction {
            from: addr(1),
            nonce: 0,
            kind: TxKind::Transfer {
                to: addr(2),
                amount: Amount::from_raw(100),
            },
            fee: 0,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        });
        assert!(!ConsensusManager::should_suppress_empty_block(&block));
    }

    // ---- Feature 132: Multi-block finality (batch catchup) ----

    #[test]
    fn batch_finalize_catchup() {
        let mut cm = ConsensusManager::new();
        let blocks = vec![
            make_test_block_with_producer(1, addr(1)),
            make_test_block_with_producer(2, addr(2)),
            make_test_block_with_producer(3, addr(3)),
        ];
        let finalized = cm.batch_finalize_catchup(blocks);
        assert_eq!(finalized.len(), 3);
        // Validator blocks should be tracked.
        assert!(cm.validator_blocks.contains_key(&(addr(1), 1)));
        assert!(cm.validator_blocks.contains_key(&(addr(2), 2)));
        assert!(cm.validator_blocks.contains_key(&(addr(3), 3)));
    }

    // ---- Feature 133: Checkpoint commitment ----

    #[test]
    fn checkpoint_commitment_consensus() {
        let mut cm = ConsensusManager::new();
        let state_root = [42u8; 32];

        // 3 validators vote for the same state root at height 1000.
        cm.record_checkpoint_vote(1000, addr(1), state_root);
        cm.record_checkpoint_vote(1000, addr(2), state_root);
        cm.record_checkpoint_vote(1000, addr(3), state_root);

        // With 4 total validators, need 3 votes (2/3 + 1 = 3).
        assert_eq!(cm.checkpoint_consensus(1000, 4), Some(state_root));

        // With 5 total validators, need 4 votes — not enough.
        assert_eq!(cm.checkpoint_consensus(1000, 5), None);
    }

    #[test]
    fn checkpoint_no_consensus_on_different_roots() {
        let mut cm = ConsensusManager::new();
        cm.record_checkpoint_vote(1000, addr(1), [1u8; 32]);
        cm.record_checkpoint_vote(1000, addr(2), [2u8; 32]);
        cm.record_checkpoint_vote(1000, addr(3), [3u8; 32]);

        // Everyone voted for different roots — no consensus.
        assert_eq!(cm.checkpoint_consensus(1000, 3), None);
    }

    // ---- Feature 137: Fork choice rule test ----

    #[test]
    fn fork_choice_crs_weighted() {
        // Create two equal-length forks. The one with CRS-weighted majority
        // should be selected through Snowball voting.
        let mut cm = ConsensusManager::new();

        // Two competing blocks at the same height from different validators.
        let block_a = make_test_block_with_producer(1, addr(1));
        let block_b = make_test_block_with_producer(1, addr(2));
        let hash_a = block_a.hash();
        let hash_b = block_b.hash();

        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        // Simulate CRS-weighted voting: validator with higher CRS gets more
        // "votes" (peers that follow CRS prefer that validator's block).
        // hash_a has 2/3 support (CRS-weighted), hash_b has 1/3.
        for _ in 0..5 {
            cm.record_response(1, hash_a);
            cm.record_response(1, hash_a);
            cm.record_response(1, hash_b);
            cm.try_finalize_round(1, 3);
        }

        let winner = cm.finalized_at_height(1);
        assert!(winner.is_some(), "Should finalize after 5 rounds of consistent voting");
        assert_eq!(winner.unwrap(), hash_a, "CRS-weighted majority should win");
    }

    #[test]
    fn fork_choice_equal_length_deterministic() {
        // With equal voting, the fork choice should still converge deterministically.
        let mut cm_1 = ConsensusManager::new();
        let mut cm_2 = ConsensusManager::new();

        let block_a = make_test_block_with_producer(1, addr(1));
        let block_b = make_test_block_with_producer(1, addr(2));
        let hash_a = block_a.hash();

        cm_1.add_candidate(block_a.clone());
        cm_1.add_candidate(block_b.clone());
        cm_2.add_candidate(block_a);
        cm_2.add_candidate(block_b);

        // Both managers see the same voting pattern.
        for _ in 0..6 {
            cm_1.record_response(1, hash_a);
            cm_1.record_response(1, hash_a);
            cm_1.try_finalize_round(1, 3);

            cm_2.record_response(1, hash_a);
            cm_2.record_response(1, hash_a);
            cm_2.try_finalize_round(1, 3);
        }

        let final_1 = cm_1.finalized_at_height(1);
        let final_2 = cm_2.finalized_at_height(1);
        assert_eq!(final_1, final_2, "Both managers must agree with same inputs");
    }

    // ---- Feature 139: Time warp attack prevention ----

    #[test]
    fn timestamp_within_drift_accepted() {
        let cm = ConsensusManager::new();
        let recent = vec![1000, 1002, 1004, 1006, 1008];
        // Median is 1004. Block at 1010 is +6s drift — within 15s limit.
        assert!(cm.validate_timestamp(1010, &recent));
    }

    #[test]
    fn timestamp_too_far_in_future_rejected() {
        let cm = ConsensusManager::new();
        let recent = vec![1000, 1002, 1004, 1006, 1008];
        // Median is 1004. Block at 1025 is +21s drift — exceeds 15s limit.
        assert!(!cm.validate_timestamp(1025, &recent));
    }

    #[test]
    fn timestamp_too_far_in_past_rejected() {
        let cm = ConsensusManager::new();
        let recent = vec![1000, 1002, 1004, 1006, 1008];
        // Median is 1004. Block at 980 is -24s drift — exceeds 15s limit.
        assert!(!cm.validate_timestamp(980, &recent));
    }

    #[test]
    fn timestamp_empty_references_always_accepted() {
        let cm = ConsensusManager::new();
        assert!(cm.validate_timestamp(9999, &[]));
    }

    // ---- Feature 131: Custom params ----

    #[test]
    fn custom_params_constructor() {
        let params = SnowballParams {
            sample_size: 10,
            quorum: 7,
            decision_threshold: 15,
        };
        let cm = ConsensusManager::with_params(params);
        // Verify custom params are used by testing convergence speed.
        // With decision_threshold=15, need 15 rounds.
        let mut cm = cm;
        let block_a = make_test_block_with_producer(1, addr(1));
        let block_b = make_test_block_with_producer(1, addr(2));
        let hash_a = block_a.hash();
        cm.add_candidate(block_a);
        cm.add_candidate(block_b);

        // 14 rounds should not be enough.
        for _ in 0..14 {
            for _ in 0..7 {
                cm.record_response(1, hash_a);
            }
            cm.try_finalize_round(1, 3);
        }
        assert_eq!(cm.finalized_at_height(1), None);

        // 1 more round should finalize.
        for _ in 0..7 {
            cm.record_response(1, hash_a);
        }
        cm.try_finalize_round(1, 3);
        assert_eq!(cm.finalized_at_height(1), Some(hash_a));
    }

    // ---- Feature 134/135: Light client proof generation and verification ----

    #[test]
    fn light_client_proof_roundtrip() {
        use commputer_core::transaction::{Transaction, TxKind};
        use commputer_core::token::Amount;

        // Create a block with transactions.
        let mut block = make_test_block(1);
        for i in 0..5 {
            let mut from = [0u8; 32];
            from[0] = i + 10;
            block.transactions.push(Transaction {
                from: Address(from),
                nonce: i as u64,
                kind: TxKind::Transfer {
                    to: addr(99),
                    amount: Amount::from_raw(100),
                },
                fee: 0,
                signature: vec![],
                public_key: vec![],
                memo: None,
                timelock: None,
            });
        }
        block.header.tx_root = block.compute_tx_root();

        // Generate proof for the 3rd transaction.
        let tx_hash = block.transactions[2].hash().0;
        let (proof, idx) = generate_light_client_proof(&block, tx_hash).unwrap();
        assert_eq!(idx, 2);

        // Verify the proof.
        assert!(verify_light_client_proof(tx_hash, &proof, block.header.tx_root));

        // Wrong tx hash should fail.
        let fake_hash = [0xFFu8; 32];
        assert!(!verify_light_client_proof(fake_hash, &proof, block.header.tx_root));
    }

    #[test]
    fn light_client_proof_missing_tx() {
        let block = make_test_block(1); // No transactions.
        let result = generate_light_client_proof(&block, [1u8; 32]);
        assert!(result.is_none());
    }

    // ---- ConsensusRoundResult enum tests ----

    #[test]
    fn real_votes_still_finalize() {
        let mut cm = ConsensusManager::new();
        let block = make_test_block(1);
        let hash = block.hash();
        let height = 1;

        cm.add_candidate(block);

        // Simulate enough rounds of unanimous votes to finalize.
        // Each round must supply at least quorum votes so record_round can converge.
        for _ in 0..cm.params.decision_threshold {
            for _ in 0..cm.params.quorum {
                cm.record_response(height, hash);
            }
            let result = cm.try_finalize_round(height, 1);
            if result == ConsensusRoundResult::Finalized {
                assert!(cm.finalized_at_height(height).is_some());
                return;
            }
        }
        panic!("expected finalization within {} rounds", cm.params.decision_threshold);
    }

    #[test]
    fn timeout_returns_stalled() {
        let mut cm = ConsensusManager::new();
        let block = make_test_block(1);
        cm.add_candidate(block);

        // Set the start time to the past so timeout triggers immediately.
        cm.height_start_time.insert(1, std::time::Instant::now() - std::time::Duration::from_secs(10));

        let result = cm.try_finalize_round(1, 0);
        assert_eq!(result, ConsensusRoundResult::Stalled);

        // Verify no finalization happened (no fabricated votes).
        assert!(cm.finalized_at_height(1).is_none());
    }

    #[test]
    fn clear_wipes_all_heights() {
        let mut cm = ConsensusManager::new();
        let block1 = make_test_block(1);
        let block2 = make_test_block(2);
        cm.add_candidate(block1);
        cm.add_candidate(block2);
        assert!(cm.has_height(1));
        assert!(cm.has_height(2));

        cm.clear();
        assert!(!cm.has_height(1));
        assert!(!cm.has_height(2));
        assert!(cm.active_heights().is_empty());
    }

    // ---- update_params_for_network_size: scaling curve ----
    // Whitepaper goal Step B: SnowballParams::production() (20/14/20) was
    // dead code; the scaler now climbs there at peer_count >= 21 instead
    // of hard-capping at sample=3. ADR-0002 documents the design.

    fn assert_curve(peer_count: usize, expect_k: usize, expect_alpha: usize, expect_beta: u32) {
        let mut cm = ConsensusManager::new();
        cm.update_params_for_network_size(peer_count);
        assert_eq!(
            cm.params.sample_size, expect_k,
            "sample_size mismatch at peer_count={peer_count}"
        );
        assert_eq!(
            cm.params.quorum, expect_alpha,
            "quorum mismatch at peer_count={peer_count}"
        );
        assert_eq!(
            cm.params.decision_threshold, expect_beta,
            "decision_threshold mismatch at peer_count={peer_count}"
        );
    }

    #[test]
    fn scaling_curve_solo_bootstrap() {
        // β=3 at 0 peers: defense in depth behind the zero-peer gate — a
        // stale-params race must not be able to one-shot finalize.
        assert_curve(0, 1, 1, 1);
    }

    #[test]
    fn scaling_curve_one_peer() {
        // n=2 voters (the peer and us): both must agree.
        assert_curve(1, 2, 2, 1);
    }

    #[test]
    fn scaling_curve_two_peers() {
        // n=3 voters: quorum 2 is a majority, so one node may be down.
        assert_curve(2, 3, 2, 1);
    }

    #[test]
    fn scaling_curve_three_peers_matches_testing_profile() {
        // n=4 voters: quorum 3 is the majority. Formerly (3,2,5), which was a
        // majority of PEERS but only 2 of 4 actual voters once our own vote is
        // counted — a tie, not a quorum.
        assert_curve(3, 4, 3, 1);
    }

    #[test]
    fn scaling_curve_five_peers_still_testing_profile() {
        assert_curve(5, 4, 3, 1);
    }

    #[test]
    fn scaling_curve_ten_peers_first_intermediate_rung() {
        assert_curve(10, 5, 4, 8);
    }

    #[test]
    fn scaling_curve_twenty_peers_second_intermediate_rung() {
        assert_curve(20, 10, 7, 14);
    }

    #[test]
    fn scaling_curve_twentyone_peers_full_production() {
        assert_curve(21, 20, 14, 20);
    }

    #[test]
    fn scaling_curve_hundred_peers_full_production() {
        assert_curve(100, 20, 14, 20);
    }

    #[test]
    fn scaling_curve_validate_invariant_every_rung() {
        // For every rung where sample > 1, quorum must be > sample/2 (liveness).
        // For sample == 1 (peer_count 0 and 1), validate() requires α >= 1
        // since k/2 == 0.
        let cases = [0usize, 1, 2, 3, 5, 10, 20, 21, 100];
        for &pc in &cases {
            let mut cm = ConsensusManager::new();
            cm.update_params_for_network_size(pc);
            let k = cm.params.sample_size;
            let alpha = cm.params.quorum;
            assert!(alpha >= 1, "alpha must be >= 1 at peer_count={pc}");
            assert!(alpha <= k, "alpha must be <= k at peer_count={pc}");
            if k > 1 {
                assert!(
                    alpha > k / 2,
                    "alpha ({alpha}) must be > k/2 ({}) at peer_count={pc}",
                    k / 2
                );
            }
            assert!(cm.params.decision_threshold >= 1);
        }
    }

    #[test]
    fn scaling_curve_propagates_to_existing_voters() {
        // Regression: the propagation loop must keep stale voters in sync.
        // Adding a candidate creates a voter at (3,2,5) defaults; bumping
        // peer_count to 100 should re-parameterise that voter to production
        // (20,14,20) — proven by 5 unanimous rounds NOT being enough to
        // finalize at the larger β.
        let mut cm = ConsensusManager::new();
        let block = make_test_block(1);
        cm.add_candidate(block);
        cm.update_params_for_network_size(100);
        let hash = cm.heights.get(&1).unwrap().voter.preference().unwrap();
        for _ in 0..5 {
            for _ in 0..14 {
                cm.record_response(1, hash);
            }
            cm.try_finalize_round(1, 100);
        }
        assert!(
            cm.finalized_at_height(1).is_none(),
            "5 rounds should not be enough at production beta=20"
        );
    }

    // ---- Security: unbounded-growth bounds (heights / candidates / checkpoints) ----

    #[test]
    fn far_future_candidate_height_rejected() {
        let mut cm = ConsensusManager::new();
        // applied_tip defaults to 0; a candidate far beyond the window must be
        // dropped so an attacker cannot flood arbitrary heights into `heights`.
        let far = super::MAX_HEIGHT_WINDOW + 5_000;
        cm.add_candidate(make_test_block(far));
        assert!(!cm.has_height(far), "candidate far above the applied tip must not be tracked");
        // A near-tip candidate is still accepted.
        cm.add_candidate(make_test_block(1));
        assert!(cm.has_height(1), "near-tip candidate must be tracked");
    }

    #[test]
    fn stale_candidate_height_rejected_after_cleanup() {
        let mut cm = ConsensusManager::new();
        cm.add_candidate(make_test_block(5));
        assert!(cm.has_height(5));
        // Apply up to height 10 — heights <= 10 are pruned and become stale.
        cm.cleanup_below(10);
        assert!(!cm.has_height(5));
        // A new candidate at/below the applied tip must be rejected.
        cm.add_candidate(make_test_block(8));
        assert!(!cm.has_height(8), "candidate at/below applied tip must be rejected");
        // But a fresh candidate above the tip is accepted.
        cm.add_candidate(make_test_block(11));
        assert!(cm.has_height(11));
    }

    #[test]
    fn candidates_per_height_capped() {
        let mut cm = ConsensusManager::new();
        // Mint many distinct candidate hashes at a single near-tip height by
        // varying producer + timestamp. Only MAX_CANDIDATES_PER_HEIGHT are kept.
        let n = super::MAX_CANDIDATES_PER_HEIGHT as u64 + 200;
        for i in 0..n {
            let mut a = [0u8; 32];
            a[0] = (i % 251) as u8;
            a[1] = (i / 251) as u8;
            cm.add_candidate(make_test_block_with_timestamp(1, Address(a), 1000 + i));
        }
        assert_eq!(
            cm.candidates_at_height(1),
            super::MAX_CANDIDATES_PER_HEIGHT,
            "distinct candidates at one height must be capped"
        );
        // validator_blocks for this height must also be bounded by the cap.
        let vb = cm.validator_blocks.keys().filter(|(_, h)| *h == 1).count();
        assert!(
            vb <= super::MAX_CANDIDATES_PER_HEIGHT,
            "validator_blocks at one height must be bounded, got {vb}"
        );
    }

    #[test]
    fn checkpoint_votes_pruned_on_cleanup() {
        let mut cm = ConsensusManager::new();
        cm.record_checkpoint_vote(5, addr(1), [1u8; 32]);
        cm.record_checkpoint_vote(6, addr(2), [2u8; 32]);
        assert!(cm.checkpoint_votes.contains_key(&5));
        // Applying up to height 10 prunes checkpoint votes for finalized heights.
        cm.cleanup_below(10);
        assert!(
            !cm.checkpoint_votes.contains_key(&5),
            "checkpoint votes for finalized heights must be pruned"
        );
        assert!(!cm.checkpoint_votes.contains_key(&6));
    }

    #[test]
    fn checkpoint_far_future_height_rejected() {
        let mut cm = ConsensusManager::new();
        cm.record_checkpoint_vote(super::MAX_HEIGHT_WINDOW + 10_000, addr(1), [1u8; 32]);
        assert!(
            cm.checkpoint_votes.is_empty(),
            "checkpoint vote far above the tip must be ignored"
        );
    }

    #[test]
    fn checkpoint_validators_per_height_capped() {
        let mut cm = ConsensusManager::new();
        // One height, many synthesised validator addresses.
        let n = super::MAX_CHECKPOINT_VALIDATORS_PER_HEIGHT + 500;
        for i in 0..n {
            let mut a = [0u8; 32];
            a[0] = (i % 251) as u8;
            a[1] = ((i / 251) % 251) as u8;
            a[2] = (i / (251 * 251)) as u8;
            cm.record_checkpoint_vote(1, Address(a), [7u8; 32]);
        }
        let tracked = cm.checkpoint_votes.get(&1).map(|m| m.len()).unwrap_or(0);
        assert_eq!(
            tracked,
            super::MAX_CHECKPOINT_VALIDATORS_PER_HEIGHT,
            "distinct checkpoint validators at one height must be capped"
        );
    }

    /// A tx packed into a LOSING candidate must be surrendered by the take so
    /// the caller can requeue it — otherwise it is destroyed with the dropped
    /// candidate (live finding: every seed-submitted faucet dispense died this
    /// way, because the WAN-lagged seed loses essentially every proposal race).
    #[test]
    fn take_finalized_with_lost_surrenders_losing_candidates_txs() {
        let mut cm = ConsensusManager::new();
        cm.cleanup_below(4);
        let tip = BlockHash([7u8; 32]);

        let mut winner = make_test_block_with_producer(5, addr(1));
        winner.header.parent_hash = tip;
        let mut loser = make_test_block_with_producer(5, addr(2));
        loser.header.parent_hash = tip;
        loser.transactions.push(Transaction {
            from: addr(9),
            nonce: 0,
            kind: commputer_core::transaction::TxKind::Transfer {
                to: addr(8),
                amount: commputer_core::token::Amount::from_raw(1),
            },
            fee: commputer_core::transaction::MINIMUM_FEE,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        });
        let lost_tx_hash = loser.transactions[0].hash();

        cm.add_candidate(winner.clone());
        cm.add_candidate(loser.clone());
        // Finalize the WINNER via peer votes (quorum 2, beta drives itself).
        let (pa, pb) = (PeerId::random(), PeerId::random());
        for _ in 0..10 {
            cm.record_peer_response(5, winner.hash(), pa);
            cm.record_peer_response(5, winner.hash(), pb);
            cm.try_finalize_round(5, 2);
        }
        assert_eq!(cm.finalized_at_height(5), Some(winner.hash()));

        let (block, lost) = cm
            .take_finalized_with_lost(5)
            .expect("winner has a body, take must succeed");
        assert_eq!(block.hash(), winner.hash());
        assert_eq!(
            lost.iter().map(|t| t.hash()).collect::<Vec<_>>(),
            vec![lost_tx_hash],
            "the losing candidate's tx must be surrendered for requeue, not dropped"
        );

        // The round is consumed either way — a second take yields nothing.
        assert!(cm.take_finalized_with_lost(5).is_none());
    }

    /// The SYNC twin: a height applied via sync never goes through
    /// take_finalized, so its losing candidates' txs must be surrendered
    /// explicitly before cleanup_below destroys them.
    #[test]
    fn surrender_lost_at_returns_losers_txs_and_consumes_the_round() {
        let mut cm = ConsensusManager::new();
        cm.cleanup_below(4);
        let tip = BlockHash([7u8; 32]);

        let mut applied = make_test_block_with_producer(5, addr(1));
        applied.header.parent_hash = tip;
        let mut loser = make_test_block_with_producer(5, addr(2));
        loser.header.parent_hash = tip;
        loser.transactions.push(Transaction {
            from: addr(9),
            nonce: 0,
            kind: commputer_core::transaction::TxKind::Transfer {
                to: addr(8),
                amount: commputer_core::token::Amount::from_raw(1),
            },
            fee: commputer_core::transaction::MINIMUM_FEE,
            signature: vec![],
            public_key: vec![],
            memo: None,
            timelock: None,
        });
        let lost_tx_hash = loser.transactions[0].hash();

        cm.add_candidate(applied.clone());
        cm.add_candidate(loser);

        // No finalization required — sync applies the block regardless of the
        // local round state.
        let lost = cm.surrender_lost_at(5, &applied.hash());
        assert_eq!(
            lost.iter().map(|t| t.hash()).collect::<Vec<_>>(),
            vec![lost_tx_hash]
        );
        assert!(!cm.has_height(5), "round state consumed, matching take semantics");
        // A height we never tracked surrenders nothing.
        assert!(cm.surrender_lost_at(99, &applied.hash()).is_empty());
    }

    /// The per-height vote LOCK is what makes one-round finality (beta=1) safe.
    /// Without it, a node answering "lowest candidate hash" would switch its
    /// vote the moment a lower-hash candidate arrived — so one node could help
    /// two different blocks reach a majority at the same height. Once we have
    /// voted, our answer must not move.
    #[test]
    fn vote_choice_locks_and_does_not_switch_to_a_lower_hash() {
        let mut cm = ConsensusManager::new();
        cm.cleanup_below(4);
        let tip = BlockHash([7u8; 32]);

        let mut first = make_test_block_with_producer(5, addr(9));
        first.header.parent_hash = tip;
        cm.add_candidate(first.clone());
        let locked = cm.query_votable_preference(5, tip).expect("votes for the only candidate");
        assert_eq!(locked, first.hash());

        // A second candidate arrives; find one that hashes LOWER so the
        // unlocked rule would prefer it.
        let mut lower: Option<Block> = None;
        for n in 1..40u8 {
            let mut b = make_test_block_with_producer(5, addr(n));
            b.header.parent_hash = tip;
            if b.hash() < first.hash() {
                lower = Some(b);
                break;
            }
        }
        let lower = lower.expect("a lower-hash candidate exists");
        cm.add_candidate(lower.clone());
        assert!(lower.hash() < first.hash());

        assert_eq!(
            cm.query_votable_preference(5, tip),
            Some(first.hash()),
            "must keep voting for the locked choice even though a lower hash arrived"
        );
    }

    /// A quorum must count OUR OWN vote, not just peers'. The aggregator holds
    /// peer responses only, so with two peers a quorum of 2 demanded unanimity
    /// and one slow or restarting peer froze every round — the shape behind the
    /// live 2026-07-25 fork/stall. Counting ourselves makes it a 2-of-3
    /// majority that tolerates one node being down.
    #[test]
    fn quorum_counts_our_own_vote_so_one_peer_suffices_at_three_nodes() {
        let mut cm = ConsensusManager::new();
        cm.cleanup_below(4);
        cm.update_params_for_network_size(2); // 2 peers ⇒ quorum 2
        assert_eq!(cm.params.quorum, 2);

        let block = make_test_block_with_producer(5, addr(1));
        cm.add_candidate(block.clone());

        // ONE peer votes (the other is down). Our own preference is the same
        // candidate, so the pair of us is a majority of the three validators.
        let only_peer = PeerId::random();
        let mut finalized = false;
        for _ in 0..10 {
            cm.record_peer_response(5, block.hash(), only_peer);
            if cm.try_finalize_round(5, 2) == ConsensusRoundResult::Finalized {
                finalized = true;
                break;
            }
        }
        assert!(
            finalized,
            "one peer plus our own vote must reach quorum 2; before this fix \
             the tally held only the peer's vote and the round never finalized"
        );
        assert_eq!(cm.finalized_at_height(5), Some(block.hash()));
    }

    /// The self-vote must not manufacture a quorum on its own: with no peer
    /// responses there is no round at all, so an isolated node cannot count
    /// itself to finality.
    #[test]
    fn self_vote_alone_never_finalizes() {
        let mut cm = ConsensusManager::new();
        cm.cleanup_below(4);
        cm.update_params_for_network_size(2);
        cm.add_candidate(make_test_block_with_producer(5, addr(1)));
        for _ in 0..10 {
            assert_ne!(
                cm.try_finalize_round(5, 2),
                ConsensusRoundResult::Finalized,
                "no peer votes ⇒ no round ⇒ no finalization"
            );
        }
        assert!(cm.finalized_at_height(5).is_none());
    }

    /// Vote-height discipline: a node may only endorse a candidate that builds
    /// on the chain it has actually applied. Endorsing anything else is a
    /// rubber-stamp — the voter cannot have validated a block whose parent it
    /// does not hold — and that is what let a producer finalize 17 blocks
    /// ahead of its quorum live.
    #[test]
    fn votable_preference_requires_parent_to_be_our_tip() {
        let mut cm = ConsensusManager::new();
        cm.cleanup_below(4);
        let our_tip = BlockHash([7u8; 32]);

        // A candidate at tip+1 that extends OUR tip: votable.
        let mut good = make_test_block_with_producer(5, addr(1));
        good.header.parent_hash = our_tip;
        cm.add_candidate(good.clone());
        assert_eq!(
            cm.query_votable_preference(5, our_tip),
            Some(good.hash()),
            "a candidate building on our tip is votable"
        );

        // A candidate built on somebody else's chain: NOT votable, even though
        // the plain preference would happily return something.
        let foreign_tip = BlockHash([9u8; 32]);
        assert_eq!(
            cm.query_votable_preference(5, foreign_tip),
            None,
            "no candidate extends that tip — must answer NotReady, not a vote"
        );
        assert!(
            cm.query_preference(5).is_some(),
            "the old unconditional path would still have voted here"
        );
    }

    /// Among several candidates that all extend our tip, the votable choice is
    /// deterministic (lowest hash) so a symmetric set converges instead of
    /// splitting — same rule as the initial-preference tie-break.
    #[test]
    fn votable_preference_is_deterministic_across_candidates() {
        let our_tip = BlockHash([7u8; 32]);
        let mut a = make_test_block_with_producer(5, addr(1));
        a.header.parent_hash = our_tip;
        let mut b = make_test_block_with_producer(5, addr(2));
        b.header.parent_hash = our_tip;
        let lowest = a.hash().min(b.hash());

        let mut ab = ConsensusManager::new();
        ab.cleanup_below(4);
        ab.add_candidate(a.clone());
        ab.add_candidate(b.clone());

        let mut ba = ConsensusManager::new();
        ba.cleanup_below(4);
        ba.add_candidate(b);
        ba.add_candidate(a);

        assert_eq!(ab.query_votable_preference(5, our_tip), Some(lowest));
        assert_eq!(ba.query_votable_preference(5, our_tip), Some(lowest));
    }

    /// A quorum can form around a hash whose block BODY we never received
    /// (votes carry hashes, not bodies). take_finalized must then leave the
    /// round intact — destroying it strands the quorum and locks the producer
    /// out until resync (alpha.6 panel: free producer-lockout).
    #[test]
    fn take_finalized_preserves_round_when_body_absent() {
        let mut cm = ConsensusManager::new();
        cm.cleanup_below(4);
        let ours = make_test_block_with_producer(5, addr(1));
        cm.add_candidate(ours.clone());
        // Drive the voter to finalize on a FOREIGN hash we hold no body for.
        let foreign = make_test_block_with_producer(5, addr(9)).hash();
        let (pa, pb) = (PeerId::random(), PeerId::random());
        for _ in 0..10 {
            cm.record_peer_response(5, foreign, pa);
            cm.record_peer_response(5, foreign, pb);
            cm.try_finalize_round(5, 2);
        }
        if cm.finalized_at_height(5) == Some(foreign) {
            assert!(cm.take_finalized(5).is_none(), "no body -> no block");
            assert!(cm.has_height(5), "round state survives for the late-arriving body");
        }
    }

    /// Deterministic symmetric tie-break: whatever order candidates arrive,
    /// the un-voted initial preference converges on the LOWEST hash — a
    /// fully-meshed set of competing producers otherwise deadlocks with each
    /// node preferring its own first-arrived block (live 2026-07-25: the
    /// all-alpha.5 triangle froze at height 31).
    #[test]
    fn initial_preference_converges_on_lowest_candidate_hash() {
        // Distinct producers ⇒ distinct hashes at the same height.
        let a = make_test_block_with_producer(5, addr(1));
        let b = make_test_block_with_producer(5, addr(2));
        let lowest = a.hash().min(b.hash());

        let mut first_ab = ConsensusManager::new();
        first_ab.cleanup_below(4);
        first_ab.add_candidate(a.clone());
        first_ab.add_candidate(b.clone());
        assert_eq!(first_ab.query_preference(5), Some(lowest));

        // Reverse arrival order must land on the same preference.
        let mut first_ba = ConsensusManager::new();
        first_ba.cleanup_below(4);
        first_ba.add_candidate(b);
        first_ba.add_candidate(a);
        assert_eq!(first_ba.query_preference(5), Some(lowest));
    }

    // ---- Zero-peer solo-finalize gate (alpha.5 formation hardening) ----
    // At 0 peers the solo rung let a node self-vote its own candidate to
    // finalization every stall timeout, minting a private fork (observed 4x
    // live, 2026-07-24). Only the genesis ceremony — block 1 on a virgin
    // chain (applied_tip == 0) — may finalize alone.

    #[test]
    fn established_node_zero_peers_returns_not_ready_not_stalled() {
        let mut cm = ConsensusManager::new();
        cm.cleanup_below(4); // established chain: applied_tip = 4
        cm.add_candidate(make_test_block(5));
        // Backdate so the stall timeout would otherwise fire.
        cm.height_start_time
            .insert(5, Instant::now() - std::time::Duration::from_secs(60));
        assert_eq!(cm.try_finalize_round(5, 0), ConsensusRoundResult::NotReady);
        assert!(cm.finalized_at_height(5).is_none());
    }

    #[test]
    fn zero_peer_gate_preserves_pending_votes_for_reconnect() {
        let mut cm = ConsensusManager::new();
        cm.cleanup_below(4);
        let block = make_test_block(5);
        let hash = block.hash();
        cm.add_candidate(block);

        let peer = PeerId::random();
        assert!(cm.record_peer_response(5, hash, peer));
        // Gate fires at 0 peers but must NOT consume the pending round.
        assert_eq!(cm.try_finalize_round(5, 0), ConsensusRoundResult::NotReady);
        // Same peer re-voting is still deduped -> aggregator survived the gate.
        assert!(!cm.record_peer_response(5, hash, peer));

        // Peer connectivity returns: preserved + fresh votes finalize normally.
        for _ in 0..5 {
            cm.record_response(5, hash);
            cm.record_response(5, hash);
            cm.try_finalize_round(5, 3);
        }
        assert_eq!(cm.finalized_at_height(5), Some(hash));
    }

    #[test]
    fn genesis_bootstrap_still_solo_finalizes_block_1() {
        // Virgin chain (applied_tip == 0), height 1: the ceremony path must
        // still reach Stalled — the event loop's self-vote trigger.
        let mut cm = ConsensusManager::new();
        cm.add_candidate(make_test_block(1));
        cm.height_start_time
            .insert(1, Instant::now() - std::time::Duration::from_secs(60));
        assert_eq!(cm.try_finalize_round(1, 0), ConsensusRoundResult::Stalled);
    }

    #[test]
    fn resync_clear_does_not_reopen_solo_bootstrap() {
        let mut cm = ConsensusManager::new();
        cm.cleanup_below(4); // chain had advanced before the resync
        cm.clear();
        cm.add_candidate(make_test_block(5));
        cm.height_start_time
            .insert(5, Instant::now() - std::time::Duration::from_secs(60));
        // applied_tip survives clear(): still an established node, still gated.
        assert_eq!(cm.try_finalize_round(5, 0), ConsensusRoundResult::NotReady);
    }

    #[test]
    fn allow_solo_override_restores_stalled_at_zero_peers() {
        let mut cm = ConsensusManager::new();
        cm.cleanup_below(4);
        cm.allow_solo = true; // explicit ceremony bootstrap wiring
        cm.add_candidate(make_test_block(5));
        cm.height_start_time
            .insert(5, Instant::now() - std::time::Duration::from_secs(60));
        assert_eq!(cm.try_finalize_round(5, 0), ConsensusRoundResult::Stalled);
    }

    // ---- Per-peer NotReady stall-reset budget ----

    #[test]
    fn notready_budget_per_peer_exhausts_at_5() {
        let mut cm = ConsensusManager::new();
        let peer = PeerId::random();
        for _ in 0..MAX_NOTREADY_STALL_RESETS {
            assert!(cm.allow_notready_stall_reset(peer));
        }
        assert!(!cm.allow_notready_stall_reset(peer));
        assert!(!cm.allow_notready_stall_reset(peer)); // stays exhausted
    }

    #[test]
    fn notready_budget_second_peer_has_own_budget() {
        let mut cm = ConsensusManager::new();
        let hostile = PeerId::random();
        for _ in 0..MAX_NOTREADY_STALL_RESETS {
            assert!(cm.allow_notready_stall_reset(hostile));
        }
        assert!(!cm.allow_notready_stall_reset(hostile));
        // A different (honest, syncing) peer keeps its own full budget.
        let honest = PeerId::random();
        for _ in 0..MAX_NOTREADY_STALL_RESETS {
            assert!(cm.allow_notready_stall_reset(honest));
        }
        assert!(!cm.allow_notready_stall_reset(honest));
    }

    #[test]
    fn notready_budget_replenished_on_tip_advance() {
        let mut cm = ConsensusManager::new();
        let peer = PeerId::random();
        for _ in 0..MAX_NOTREADY_STALL_RESETS {
            assert!(cm.allow_notready_stall_reset(peer));
        }
        assert!(!cm.allow_notready_stall_reset(peer));
        // A cleanup at the same tip is not an advancement -> no replenish.
        cm.cleanup_below(0);
        assert!(!cm.allow_notready_stall_reset(peer));
        // Tip advances -> the chain is making progress; budget replenished.
        cm.cleanup_below(1);
        assert!(cm.allow_notready_stall_reset(peer));
    }

    #[test]
    fn notready_budget_cap_denies_65th_peer() {
        let mut cm = ConsensusManager::new();
        for _ in 0..MAX_NOTREADY_STALL_RESET_PEERS {
            assert!(cm.allow_notready_stall_reset(PeerId::random()));
        }
        // Map full: an unseen peer is denied rather than evicting an entry.
        assert!(!cm.allow_notready_stall_reset(PeerId::random()));
        // clear() (resync path) replenishes the map.
        cm.clear();
        assert!(cm.allow_notready_stall_reset(PeerId::random()));
    }
}
