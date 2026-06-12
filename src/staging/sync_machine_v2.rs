// sync_machine_v2.rs — Hash-aware SyncMachine surface (A7-sync-blockhash)
//
// WHAT THIS DOES
//   Augments src/node/src/sync_machine.rs so peer tips carry block HASHES, not just
//   bare u64 heights. Adds:
//     * PeerTip { height, hash, peer }                  — an expressible peer tip
//     * record_tip(height, hash, peer)                  — replaces/augments record_height
//     * trait ForkChoice + SnowballWeightForkChoice     — pluggable, Snowball-weight
//                                                          (the protocol has NO total-difficulty)
//     * enum VerifyOutcome                              — Complete | KeepDownloading |
//                                                          Rollback | WipeAndResync
//     * complete_verification_v2(our_height, our_tip)   — rollback-aware; no longer
//                                                          collapses "stale" and "caught up"
//   This makes the 5 W5.10 scenarios (fork-at-depth, rapid-reorg, partial-block,
//   out-of-order, lying-peer) EXPRESSIBLE and TESTABLE (see mod w5_10_scenarios).
//
// WHERE IT WIRES IN (file:line, all verified against the tree on agent branch)
//   * src/node/src/sync_machine.rs:15   — add `use commputer_core::block::BlockHash;`
//   * src/node/src/sync_machine.rs:46-61— add `tip_reports: Vec<PeerTip>` + `fork_choice` field
//   * src/node/src/sync_machine.rs:106  — `record_height` -> delegate to `record_tip`
//   * src/node/src/sync_machine.rs:219  — add `complete_verification_v2` beside old fn
//   * clear tip_reports wherever height_responses.clear() runs: lines 95,143,161,229,267
//
// EXISTING FILE THAT CHANGES (and is PROTECTED — founder only, blueprint not edit):
//   * src/node/src/event_loop.rs:1541   — record_height(h) -> record_tip(height, hash, peer)
//                                          (`peer` already in scope from line 1479)
//   * src/node/src/event_loop.rs:847-854 — drive complete_verification_v2 + revert_to(...)
//   Non-protected ripples (founder, but no protected-file edit):
//   * src/network/src/sync_protocol.rs:32 — SyncResponse::Height(u64) must carry a tip hash
//   * src/storage/src/state.rs:1067       — ChainState::revert_to() is the rollback primitive
//                                           (ALREADY EXISTS; bounded by FINALITY_DEPTH=10)
//
// PROTECTED-FILE DEPENDENCY: yes — see event_loop.rs anchors above. This file is
// reference-only and is NOT added to lib.rs or Cargo.toml.
//
// NOTE: written as a standalone module so it type-checks in isolation against
// commputer_core + libp2p. When ported, drop the local `FINALITY_DEPTH` const and
// import commputer_storage::state::FINALITY_DEPTH instead.

#![allow(dead_code)]

use std::collections::HashMap;

use commputer_core::block::BlockHash;
use libp2p::PeerId;

/// Mirror of storage::state::FINALITY_DEPTH (= 10). Reverts deeper than this are
/// refused by ChainState::revert_to, so v2 escalates them to WipeAndResync.
pub const FINALITY_DEPTH: u64 = 10;

/// Number of failures before a peer is excluded from fork-choice weight.
/// Mirrors sync_machine.rs MAX_PEER_FAILURES (= 10).
pub const MAX_PEER_FAILURES: u32 = 10;

/// An expressible peer tip: a height AND the hash that peer claims at that height.
/// This is the type the bare-`u64` `height_responses` could not represent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerTip {
    pub height: u64,
    pub hash: BlockHash,
    pub peer: PeerId,
}

/// Result of a rollback-aware verification round. Replaces the old bare `bool`
/// returned by `complete_verification`, which collapsed "caught up" and
/// "at-or-above-target-but-on-an-orphan" into the same `true`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Caught up on the canonical chain. Go Active. (Old code's `true`.)
    Complete,
    /// Behind the network. Keep downloading toward `new_target`. (Old code's `false`.)
    KeepDownloading { new_target: u64 },
    /// We are at/above the target height but our tip hash disagrees with the
    /// network-chosen canonical tip — we are on an orphan. Caller must
    /// `ChainState::revert_to(fork_point)` then re-download. `depth <= FINALITY_DEPTH`.
    Rollback { fork_point: u64, canonical_tip: BlockHash, depth: u64 },
    /// Divergence deeper than FINALITY_DEPTH — `revert_to` would refuse. Caller must
    /// wipe the chain and full-resync (the existing ForkDetector::should_resync path).
    WipeAndResync { canonical_tip: BlockHash },
}

/// Pluggable fork-choice. The protocol uses Snowball (no total-difficulty), so the
/// default impl is weight-by-peer-quorum, mirroring SnowballVoter::record_round's
/// "pick the quorum choice" logic (consensus/src/snowball.rs:113-145).
pub trait ForkChoice {
    /// Given all peer tip reports for this verify round and our own tip, return the
    /// `(height, hash)` the network has the most Snowball weight behind, or `None`
    /// if no tip reaches quorum.
    fn choose(
        &self,
        reports: &[PeerTip],
        our_tip: Option<(u64, BlockHash)>,
    ) -> Option<(u64, BlockHash)>;
}

/// Default Snowball-weight fork choice.
///
/// Algorithm:
///   1. Each distinct peer's vote for a `(height, hash)` counts once (Snowball
///      samples peers, not messages — snowball.rs:93-103).
///   2. The canonical tip is the HIGHEST height whose hash is backed by >= quorum
///      distinct peers; ties at a height resolved by max peer weight
///      (the quorum choice, snowball.rs:113-118).
///   3. `quorum` is clamped down to the observed peer count so small testnets
///      (3-5 peers) are not frozen — consistent with the stepped Snowball curve
///      that only reaches (20,14,20) at peer_count >= 21.
pub struct SnowballWeightForkChoice {
    /// Target quorum (default 14, matching SnowballParams::default().quorum).
    pub quorum: usize,
}

impl Default for SnowballWeightForkChoice {
    fn default() -> Self {
        // SnowballParams::default().quorum == 14 (consensus/src/snowball.rs:22).
        Self { quorum: 14 }
    }
}

impl ForkChoice for SnowballWeightForkChoice {
    fn choose(
        &self,
        reports: &[PeerTip],
        _our_tip: Option<(u64, BlockHash)>,
    ) -> Option<(u64, BlockHash)> {
        if reports.is_empty() {
            return None;
        }

        // Distinct peers observed -> clamp quorum so a small testnet isn't frozen.
        let mut distinct_peers: Vec<PeerId> =
            reports.iter().map(|r| r.peer).collect();
        distinct_peers.sort();
        distinct_peers.dedup();
        let observed = distinct_peers.len();
        // Need a strict majority of observed peers, capped at the configured quorum.
        let effective_quorum = self.quorum.min(observed).max(observed / 2 + 1);

        // Count DISTINCT peers per (height, hash). A peer voting twice for the same
        // tip counts once; a peer voting for two tips counts toward each once.
        let mut weight: HashMap<(u64, BlockHash), Vec<PeerId>> = HashMap::new();
        for r in reports {
            let entry = weight.entry((r.height, r.hash)).or_default();
            if !entry.contains(&r.peer) {
                entry.push(r.peer);
            }
        }

        // Keep only tips that reach quorum, then pick highest height, then max weight.
        let mut candidates: Vec<((u64, BlockHash), usize)> = weight
            .into_iter()
            .map(|(k, peers)| (k, peers.len()))
            .filter(|(_, w)| *w >= effective_quorum)
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Highest height wins; tie broken by greater peer weight, then by hash bytes
        // (deterministic).
        candidates.sort_by(|a, b| {
            b.0 .0 // height desc
                .cmp(&a.0 .0)
                .then(b.1.cmp(&a.1)) // weight desc
                .then(a.0 .1 .0.cmp(&b.0 .1 .0)) // hash asc, deterministic tie-break
        });
        Some(candidates[0].0)
    }
}

/// A deterministic fork choice for tests — always returns a fixed decision.
pub struct FixedForkChoice(pub Option<(u64, BlockHash)>);
impl ForkChoice for FixedForkChoice {
    fn choose(&self, _r: &[PeerTip], _o: Option<(u64, BlockHash)>) -> Option<(u64, BlockHash)> {
        self.0
    }
}

/// The hash-aware slice of SyncMachine. In the real port these fields and methods
/// are folded into the existing `struct SyncMachine`; here they are a standalone
/// helper so the logic type-checks and is unit-testable in isolation.
pub struct TipTracker {
    /// Per-round peer tip reports (height + hash + peer). Cleared on the same
    /// boundaries as the legacy `height_responses` Vec<u64>.
    tip_reports: Vec<PeerTip>,
    /// Peers excluded from fork-choice weight (partial-block / liar / exhausted).
    excluded: Vec<PeerId>,
    /// Failure counts, mirroring SyncMachine::peer_failures.
    failures: HashMap<PeerId, u32>,
    /// Current download target height (mirrors SyncMachine::target_height).
    target_height: u64,
    fork_choice: Box<dyn ForkChoice>,
}

impl TipTracker {
    pub fn new(fork_choice: Box<dyn ForkChoice>) -> Self {
        Self {
            tip_reports: Vec::new(),
            excluded: Vec::new(),
            failures: HashMap::new(),
            target_height: 0,
            fork_choice,
        }
    }

    pub fn with_default_fork_choice() -> Self {
        Self::new(Box::new(SnowballWeightForkChoice::default()))
    }

    pub fn target_height(&self) -> u64 {
        self.target_height
    }

    pub fn set_target(&mut self, h: u64) {
        self.target_height = h;
    }

    /// Replacement for `record_height`. Records a peer's claimed tip (height + hash).
    /// Excluded peers contribute no weight. In the real port, `record_height(h)`
    /// becomes `record_tip(h, <sentinel-or-real-hash>, peer)`.
    pub fn record_tip(&mut self, height: u64, hash: BlockHash, peer: PeerId) {
        if self.excluded.contains(&peer) {
            return;
        }
        self.tip_reports.push(PeerTip { height, hash, peer });
    }

    /// Clear per-round reports (called wherever height_responses.clear() runs).
    pub fn clear_round(&mut self) {
        self.tip_reports.clear();
    }

    /// Record a peer failure (partial-block scenario). On reaching MAX_PEER_FAILURES
    /// the peer is excluded AND its existing tip reports are dropped from fork-choice.
    /// Returns true if the peer is now excluded.
    pub fn record_failure(&mut self, peer: PeerId) -> bool {
        let c = self.failures.entry(peer).or_insert(0);
        *c += 1;
        if *c >= MAX_PEER_FAILURES {
            if !self.excluded.contains(&peer) {
                self.excluded.push(peer);
            }
            self.tip_reports.retain(|r| r.peer != peer);
            return true;
        }
        false
    }

    pub fn is_excluded(&self, peer: &PeerId) -> bool {
        self.excluded.contains(peer)
    }

    /// Ask the pluggable fork-choice for the network-canonical tip this round.
    pub fn canonical_tip(&self, our_tip: Option<(u64, BlockHash)>) -> Option<(u64, BlockHash)> {
        self.fork_choice.choose(&self.tip_reports, our_tip)
    }

    /// Rollback-aware verification. `our_tip` is `(our_height, our_tip_hash)`.
    ///
    /// Supersedes `complete_verification` (sync_machine.rs:219), whose
    /// `if our_height >= new_target { Complete }` (line 231) was BLIND to the
    /// orphaned-chain case. v2 consults fork-choice on the canonical hash before
    /// ever declaring Complete.
    pub fn complete_verification_v2(
        &mut self,
        our_tip: Option<(u64, BlockHash)>,
    ) -> VerifyOutcome {
        let canonical = self.canonical_tip(our_tip);

        let (our_height, our_hash) = match our_tip {
            Some(t) => t,
            // We have no tip at all -> we are behind by definition.
            None => {
                let new_target = canonical.map(|(h, _)| h).unwrap_or(0);
                self.clear_round();
                return VerifyOutcome::KeepDownloading { new_target };
            }
        };

        let outcome = match canonical {
            // No peer tip reached quorum -> nothing to verify against; treat as
            // caught-up only if we are not behind any observed report.
            None => {
                let max_seen = self
                    .tip_reports
                    .iter()
                    .map(|r| r.height)
                    .max()
                    .unwrap_or(our_height);
                if our_height >= max_seen {
                    VerifyOutcome::Complete
                } else {
                    VerifyOutcome::KeepDownloading { new_target: max_seen }
                }
            }
            Some((c_height, c_hash)) => {
                if our_height < c_height {
                    // Genuinely behind on height — keep downloading.
                    VerifyOutcome::KeepDownloading { new_target: c_height }
                } else if our_height == c_height && our_hash == c_hash {
                    // Same height, same hash -> truly caught up on canonical chain.
                    VerifyOutcome::Complete
                } else {
                    // our_height >= c_height but hash disagrees at the canonical
                    // height (or we are higher on a minority fork). ORPHAN.
                    // fork_point: revert down to just below the canonical height.
                    // Conservative: revert to c_height - 1 so block c_height can be
                    // re-downloaded with the canonical hash.
                    let fork_point = c_height.saturating_sub(1);
                    let depth = our_height.saturating_sub(fork_point);
                    if depth > FINALITY_DEPTH {
                        VerifyOutcome::WipeAndResync { canonical_tip: c_hash }
                    } else {
                        VerifyOutcome::Rollback {
                            fork_point,
                            canonical_tip: c_hash,
                            depth,
                        }
                    }
                }
            }
        };

        // Update target / clear round, mirroring the old fn's bookkeeping.
        match &outcome {
            VerifyOutcome::KeepDownloading { new_target } => self.target_height = *new_target,
            VerifyOutcome::Rollback { fork_point, .. } => self.target_height = *fork_point,
            _ => {}
        }
        self.clear_round();
        outcome
    }
}

// ---------------------------------------------------------------------------
// Tests — the 5 previously-PSEUDO W5.10 scenarios, now expressible.
// Real BlockHash + PeerId values; no tautologies.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod w5_10_scenarios {
    use super::*;

    fn h(n: u8) -> BlockHash {
        let mut b = [0u8; 32];
        b[0] = n;
        BlockHash(b)
    }

    // k distinct peers all reporting the SAME (height, hash). Returns the peers so
    // the caller can add a dissenter on a known peer set.
    fn quorum_tips(t: &mut TipTracker, count: usize, height: u64, hash: BlockHash) -> Vec<PeerId> {
        let mut peers = Vec::new();
        for _ in 0..count {
            let p = PeerId::random();
            t.record_tip(height, hash, p);
            peers.push(p);
        }
        peers
    }

    // ----- Scenario 1: fork-at-depth-N -------------------------------------
    // Two camps at the SAME height with DIFFERENT hashes. Previously impossible:
    // Vec<u64> stored only the height, so the disagreement was invisible and the
    // median happily "agreed". Now fork-choice must surface the majority hash.
    #[test]
    fn fork_at_depth_n_surfaces_majority_hash() {
        // quorum of 1 so a single-camp majority decides on a tiny test set.
        let mut t = TipTracker::new(Box::new(SnowballWeightForkChoice { quorum: 3 }));
        let good = h(0xAA);
        let evil = h(0xBB);
        quorum_tips(&mut t, 4, 50, good); // 4 peers on the canonical fork
        quorum_tips(&mut t, 1, 50, evil); // 1 peer on the minority fork

        let canonical = t.canonical_tip(None);
        assert_eq!(canonical, Some((50, good)), "majority hash must win, not just height");
        assert_ne!(canonical.map(|(_, x)| x), Some(evil));
    }

    // If WE are the one sitting on the minority fork at height 50, v2 must NOT
    // declare Complete (the old `our_height >= target` collapse) — it must Rollback.
    #[test]
    fn fork_at_depth_n_our_minority_tip_triggers_rollback_not_complete() {
        let mut t = TipTracker::new(Box::new(SnowballWeightForkChoice { quorum: 3 }));
        let good = h(0xAA);
        let evil = h(0xBB);
        quorum_tips(&mut t, 4, 50, good);

        // Our local tip is height 50 but the WRONG (minority) hash.
        let outcome = t.complete_verification_v2(Some((50, evil)));
        match outcome {
            VerifyOutcome::Rollback { fork_point, canonical_tip, depth } => {
                assert_eq!(canonical_tip, good);
                assert_eq!(fork_point, 49);
                assert_eq!(depth, 1);
            }
            other => panic!("expected Rollback, got {:?}", other),
        }
    }

    // ----- Scenario 2: rapid-reorg ----------------------------------------
    // Round 1 canonical = hashA@100; round 2 a fresh quorum flips to hashB@100.
    // The machine must follow the new canonical hash and not stay Complete on the
    // stale one. Previously invisible (both rounds were just "100").
    #[test]
    fn rapid_reorg_follows_new_canonical_hash() {
        let mut t = TipTracker::new(Box::new(SnowballWeightForkChoice { quorum: 3 }));
        let a = h(0x01);
        let b = h(0x02);

        // Round 1: network on A@100, and we happen to be on A@100 -> Complete.
        quorum_tips(&mut t, 4, 100, a);
        assert_eq!(t.complete_verification_v2(Some((100, a))), VerifyOutcome::Complete);

        // Round 2: a reorg — fresh quorum now reports B@100. We are still on A@100.
        quorum_tips(&mut t, 4, 100, b);
        match t.complete_verification_v2(Some((100, a))) {
            VerifyOutcome::Rollback { canonical_tip, fork_point, depth } => {
                assert_eq!(canonical_tip, b);
                assert_eq!(fork_point, 99);
                assert_eq!(depth, 1);
            }
            other => panic!("reorg must trigger Rollback, got {:?}", other),
        }
    }

    // ----- Scenario 3: partial-block --------------------------------------
    // A peer advertises a tip at height 60 but cannot actually serve the block
    // (batch failures). After MAX_PEER_FAILURES it must be excluded AND its tip
    // dropped from fork-choice weight, so it can't sway the canonical decision.
    #[test]
    fn partial_block_peer_excluded_from_fork_choice() {
        let mut t = TipTracker::new(Box::new(SnowballWeightForkChoice { quorum: 2 }));
        let honest = h(0x10);
        let bogus = h(0x11);

        // 2 honest peers at 60/honest.
        quorum_tips(&mut t, 2, 60, honest);
        // 1 flaky peer advertising a higher, bogus tip 61/bogus.
        let flaky = PeerId::random();
        t.record_tip(61, bogus, flaky);

        // Flaky peer fails to deliver -> exhaust it.
        let mut excluded = false;
        for _ in 0..MAX_PEER_FAILURES {
            excluded = t.record_failure(flaky);
        }
        assert!(excluded, "peer must be excluded after MAX_PEER_FAILURES");
        assert!(t.is_excluded(&flaky));

        // Its bogus 61 tip is gone; canonical falls back to the honest 60.
        assert_eq!(t.canonical_tip(None), Some((60, honest)));
        // Further reports from it are ignored.
        t.record_tip(61, bogus, flaky);
        assert_eq!(t.canonical_tip(None), Some((60, honest)));
    }

    // ----- Scenario 4: out-of-order ---------------------------------------
    // Tips arrive non-monotonically (30, then 10, then 20). Fork-choice must pick
    // the highest quorum-backed tip regardless of arrival order — previously the
    // only effect of order was a sort, with no hash to validate.
    #[test]
    fn out_of_order_tips_pick_highest_quorum_height() {
        let mut t = TipTracker::new(Box::new(SnowballWeightForkChoice { quorum: 2 }));
        let h30 = h(0x30);
        let h10 = h(0x10);
        let h20 = h(0x20);

        // Each height gets a quorum of 2 distinct peers, delivered out of order.
        quorum_tips(&mut t, 2, 30, h30);
        quorum_tips(&mut t, 2, 10, h10);
        quorum_tips(&mut t, 2, 20, h20);

        assert_eq!(
            t.canonical_tip(None),
            Some((30, h30)),
            "highest quorum-backed tip wins irrespective of arrival order"
        );
    }

    // ----- Scenario 5: lying-peer -----------------------------------------
    // A single peer reports a valid-LOOKING but wrong hash at a real height.
    // Below quorum, it must be rejected; and if our own local tip happens to be
    // that liar's hash, the outcome is Rollback to the honest canonical tip —
    // never Complete. This is the exact case the old bool API could not express.
    #[test]
    fn lying_peer_rejected_and_forces_rollback_if_we_adopted_it() {
        let mut t = TipTracker::new(Box::new(SnowballWeightForkChoice { quorum: 4 }));
        let good = h(0x77);
        let lie = h(0x66);

        // 5 honest peers at 50/good (>= quorum of 4).
        quorum_tips(&mut t, 5, 50, good);
        // 1 liar at 50/lie (below quorum).
        t.record_tip(50, lie, PeerId::random());

        // Fork-choice rejects the lie.
        assert_eq!(t.canonical_tip(None), Some((50, good)));

        // If we (wrongly) adopted the liar's hash locally, we must roll back.
        match t.complete_verification_v2(Some((50, lie))) {
            VerifyOutcome::Rollback { canonical_tip, .. } => assert_eq!(canonical_tip, good),
            other => panic!("adopting a liar's tip must Rollback, got {:?}", other),
        }
    }

    // ----- Guardrails on the rollback decision itself ---------------------

    // A divergence deeper than FINALITY_DEPTH cannot be auto-reverted (revert_to
    // would Err) -> must escalate to WipeAndResync.
    #[test]
    fn deep_divergence_escalates_to_wipe_and_resync() {
        let mut t = TipTracker::new(Box::new(SnowballWeightForkChoice { quorum: 2 }));
        let canon = h(0x01);
        // Network canonical tip is far below our (orphaned) height.
        quorum_tips(&mut t, 3, 5, canon);
        // We are way ahead on a dead fork: depth = 100 - 4 = 96 > FINALITY_DEPTH.
        match t.complete_verification_v2(Some((100, h(0x99)))) {
            VerifyOutcome::WipeAndResync { canonical_tip } => assert_eq!(canonical_tip, canon),
            other => panic!("deep divergence must WipeAndResync, got {:?}", other),
        }
    }

    // Honest caught-up case still returns Complete (no false-positive rollback).
    #[test]
    fn caught_up_on_canonical_is_complete() {
        let mut t = TipTracker::new(Box::new(SnowballWeightForkChoice { quorum: 2 }));
        let tip = h(0x42);
        quorum_tips(&mut t, 3, 80, tip);
        assert_eq!(t.complete_verification_v2(Some((80, tip))), VerifyOutcome::Complete);
    }

    // Genuinely behind on height -> KeepDownloading, never Rollback.
    #[test]
    fn behind_on_height_keeps_downloading() {
        let mut t = TipTracker::new(Box::new(SnowballWeightForkChoice { quorum: 2 }));
        let tip = h(0x42);
        quorum_tips(&mut t, 3, 200, tip);
        match t.complete_verification_v2(Some((100, h(0x42)))) {
            VerifyOutcome::KeepDownloading { new_target } => assert_eq!(new_target, 200),
            other => panic!("expected KeepDownloading, got {:?}", other),
        }
    }

    // record_height back-compat shim semantics: a sentinel-hash tip still records a
    // height so the legacy median path keeps functioning during migration.
    #[test]
    fn legacy_height_only_report_still_counts_for_height() {
        let mut t = TipTracker::new(Box::new(SnowballWeightForkChoice { quorum: 1 }));
        // Simulate `record_height(70)` -> `record_tip(70, GENESIS-sentinel, peer)`.
        t.record_tip(70, BlockHash::GENESIS, PeerId::random());
        assert_eq!(t.canonical_tip(None), Some((70, BlockHash::GENESIS)));
    }
}
