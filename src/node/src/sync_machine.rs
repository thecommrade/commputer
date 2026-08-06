//! Sync state machine for the Commputer L1 blockchain.
//!
//! Manages the block synchronization lifecycle: querying peers for their
//! chain height, downloading blocks in batches, and verifying completion.
//!
//! Where to wire in: `src/node/src/event_loop.rs` — replace or augment the
//! ad-hoc sync logic with a `SyncMachine` field on `EventLoop`. Drive it
//! from the sync tick handler.
//!
//! Existing file that needs changes: `src/node/src/event_loop.rs`

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use libp2p::PeerId;
use tracing::{debug, info, warn};

/// Number of blocks to request per batch.
pub const SYNC_BATCH_SIZE: u64 = 10;

/// Seconds before a batch request is considered timed out.
pub const BATCH_TIMEOUT_SECS: u64 = 10;

/// Seconds before a height query round is considered timed out.
pub const HEIGHT_QUERY_TIMEOUT_SECS: u64 = 5;

/// Number of failures before a peer is considered exhausted.
/// QC-024: lowered from 10 so peer ROTATION happens strictly before the heavier
/// whole-machine stall reset at `MAX_STALL_BATCH_FAILURES`. See the note there —
/// while the two were equal, a peer was only ever exhausted by the same call that
/// cleared the exhausted set, so rotation never actually occurred.
pub const MAX_PEER_FAILURES: u32 = 3;

/// Security bound: the maximum number of blocks ahead of our own height that a
/// single sync cycle will target. Peer-reported heights are self-attested and
/// unauthenticated; `should_start_downloading` returns true after a single
/// response, so one fast malicious peer replying `u64::MAX` can win the
/// `begin_downloading` median race and pin `target_height` at an unreachable
/// value, wedging the node in `Downloading` forever (the reset path only fires
/// when `target == 0`). Clamping the committed target to
/// `our_height + MAX_SYNC_TARGET_GAP` keeps it reachable: a genuinely
/// far-behind node simply syncs in windows across successive verify cycles,
/// while a bogus height can no longer create a permanent stall. Generous — far
/// larger than any honest single-cycle catch-up gap.
pub const MAX_SYNC_TARGET_GAP: u64 = 100_000;

/// Maximum number of `request_block` calls the out-of-order sync path may issue
/// in a SINGLE burst when it observes a block whose height is ahead of the tip.
/// Bounds the gap-request loop in `apply_synced_block` / `try_apply_finalized`
/// (SECURITY finding [1]): without it, a peer answering with header
/// `height = u64::MAX` drives ~1.8e19 synchronous `request_block` iterations on
/// the single-threaded event loop = permanent freeze + outbound amplification.
/// One burst is bounded to a batch; the loop re-fires on later blocks so a
/// genuinely far-behind node still catches up across successive ticks.
/// INERT until the protected event_loop gap-request hunks reference it.
pub const MAX_SYNC_GAP: u64 = SYNC_BATCH_SIZE;

/// Liveness watchdog bound: the number of consecutive batch failures — while
/// `Downloading` toward a nonzero target and WITHOUT our height advancing — after
/// which the machine tears down the current sync attempt and re-queries fresh
/// peer heights.
///
/// The `MAX_SYNC_TARGET_GAP` clamp keeps a bogus target *reachable* but does not
/// keep it *true*: on a short alpha chain a single malicious peer can still win
/// the median race and pin `target_height` ABOVE the real tip yet below the
/// clamp. The node then downloads the whole real chain and wedges — `next_batch`
/// never returns `None` (our_height < target forever), so `Verifying` (the only
/// path that re-queries heights) is never entered, and the `target == 0` reset is
/// never reached. Every further batch request for a block past the true tip times
/// out. Counting those no-progress failures and resetting once they cross this
/// bound breaks the wedge locally: the reset returns the machine to its initial
/// state so the event loop re-queries heights on the next tick, where an honest
/// majority can correct the target. Any forward progress (a batch that advances
/// our height) resets the counter, so a slow-but-genuine sync never trips it.
///
/// QC-024: this used to be pinned EQUAL to `MAX_PEER_FAILURES`, because once a
/// lone peer was exhausted `select_peer` returned `None`, no further failures
/// could ever be recorded, and the reset therefore had to fire on that same
/// failure or never. `select_peer` now FAILS OPEN (it retries a previously-failed
/// peer rather than returning `None`), so failures keep accruing after exhaustion
/// and that constraint is gone. Decoupling them is what makes peer ROTATION real:
/// with both at 10, a peer was only ever marked exhausted by the very call that
/// also reset and cleared `exhausted_peers`, so `select_peer` always saw an empty
/// exhausted set and always answered `first()`. Rotating at 3 lets a healthy
/// second peer be tried well before the heavier whole-machine reset at 10.
pub const MAX_STALL_BATCH_FAILURES: u32 = 10;

/// Re-engagement: minimum blocks behind the network tip before the standing
/// re-sync check arms. A 1-block lag is normal gossip skew around a moving
/// tip; a sustained lag of 2+ means we are genuinely falling behind.
pub const MIN_REENGAGE_LAG: u64 = 2;

/// Re-engagement: the lag must persist this long (from first observation)
/// before a fire, so a transient spike — a block observed while its gossip is
/// still mid-flight to us — never triggers a resync round.
pub const REENGAGE_GRACE_SECS: u64 = 5;

/// Re-engagement backoff ladder base: seconds required between the 1st and 2nd
/// fire; doubles per subsequent fire (30s, 60s, 120s, …).
pub const REENGAGE_BACKOFF_BASE_SECS: u64 = 30;

/// Re-engagement backoff ladder cap: fires are never spaced more than this far
/// apart, so a persistently-lagging node keeps retrying at a bounded,
/// non-spammy rate instead of backing off forever.
pub const REENGAGE_BACKOFF_CAP_SECS: u64 = 300;

/// States of the sync lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncState {
    /// Not syncing. Waiting to be started or already up to date.
    Idle,
    /// Querying peers for their current chain height.
    QueryHeight,
    /// Downloading blocks from peers in batches.
    Downloading,
    /// Verifying that we have caught up; re-checking peer heights.
    Verifying,
    /// Sync is complete — we are at the target height.
    Complete,
}

/// State machine that drives the block sync lifecycle.
pub struct SyncMachine {
    /// Current state.
    state: SyncState,
    /// The height we are trying to reach.
    target_height: u64,
    /// Height responses collected from peers during `QueryHeight` / `Verifying`.
    height_responses: Vec<u64>,
    /// When the current state was entered (for timeout checks).
    state_entered_at: Instant,
    /// The block range `(start, end)` currently being downloaded.
    current_batch: Option<(u64, u64)>,
    /// Number of consecutive failures per peer.
    peer_failures: HashMap<PeerId, u32>,
    /// Peers that have been permanently excluded (>= MAX_PEER_FAILURES).
    exhausted_peers: HashSet<PeerId>,
    /// Highest `our_height` observed while downloading toward the current target.
    /// Used by the liveness watchdog to detect a no-progress stall.
    last_progress_height: u64,
    /// Batch failures accumulated since the last forward progress (or since the
    /// current target was committed). Reset to 0 whenever our height advances.
    /// Drives the `MAX_STALL_BATCH_FAILURES` watchdog reset.
    consecutive_stall_failures: u32,
    /// Re-engagement: when the qualifying lag (`network >= local +
    /// MIN_REENGAGE_LAG`) was first observed, and the local height at that
    /// observation. Cleared whenever the lag breaks, the local height changes,
    /// or the machine leaves `Idle`. Survives `reset()`.
    reengage_lag_since: Option<(Instant, u64)>,
    /// Re-engagement: time and local height of the last fire. Cleared (with
    /// `reengage_fires`) once the local height advances past the recorded
    /// height. Survives `reset()` — see the note there.
    reengage_last_fire: Option<(Instant, u64)>,
    /// Fires since the last progress reset; exponent for the backoff ladder.
    reengage_fires: u32,
    /// Round contact evidence: true once at least one height probe attempt has
    /// been reported (`record_probes_sent`) or one peer height response has
    /// arrived (`record_height`) since the round started. Gates the
    /// "nothing to do" conclusions — see `begin_downloading` /
    /// `complete_verification`.
    contacted_this_round: bool,
}

impl SyncMachine {
    /// Create a new machine in the `Idle` state.
    pub fn new() -> Self {
        Self {
            state: SyncState::Idle,
            target_height: 0,
            height_responses: Vec::new(),
            state_entered_at: Instant::now(),
            current_batch: None,
            peer_failures: HashMap::new(),
            exhausted_peers: HashSet::new(),
            last_progress_height: 0,
            consecutive_stall_failures: 0,
            reengage_lag_since: None,
            reengage_last_fire: None,
            reengage_fires: 0,
            contacted_this_round: false,
        }
    }

    /// Return the current state.
    pub fn state(&self) -> &SyncState {
        &self.state
    }

    /// Return the current target height.
    pub fn target_height(&self) -> u64 {
        self.target_height
    }

    /// Transition from `Idle` or `Complete` to `QueryHeight`.
    ///
    /// Clears any stale height responses and resets the state timer.
    /// Has no effect if the machine is already in another active state.
    pub fn start(&mut self) {
        match self.state {
            SyncState::Idle | SyncState::Complete => {
                info!("[sync] starting — entering QueryHeight");
                self.height_responses.clear();
                // New round: no probes attempted, no responses heard yet.
                self.contacted_this_round = false;
                self.state = SyncState::QueryHeight;
                self.state_entered_at = Instant::now();
            }
            _ => {
                debug!("[sync] start() called in state {:?}, ignoring", self.state);
            }
        }
    }

    /// Record a height response from a peer during `QueryHeight`.
    ///
    /// Also counts as round contact for the zero-contact guard: a peer that
    /// answered is proof the round actually reached the network.
    pub fn record_height(&mut self, height: u64) {
        debug!("[sync] received height response: {}", height);
        self.height_responses.push(height);
        self.contacted_this_round = true;
    }

    /// Report that `count` height probes (`GetHeight` requests) were sent to
    /// peers for the current round.
    ///
    /// Wiring: the (protected) event loop should call this wherever it sends
    /// `SyncRequest::GetHeight` (the `Idle`-start and `Verifying` re-check
    /// probe loops), passing the number of peers actually probed. A round that
    /// probed at least one peer may conclude "nothing to do" on silence (we
    /// tried; nobody answered); a round with zero probes and zero responses may
    /// not — it is forced back through the driver's existing `target == 0`
    /// re-query path instead (see `begin_downloading`). Until wired, responses
    /// recorded via `record_height` are the only accepted contact evidence.
    pub fn record_probes_sent(&mut self, count: usize) {
        if count > 0 {
            self.contacted_this_round = true;
        }
    }

    /// Returns `true` when we should stop collecting heights and move on.
    ///
    /// True when we have at least one response, or the query timeout has elapsed.
    pub fn should_start_downloading(&self, our_height: u64) -> bool {
        let _ = our_height;
        if !self.height_responses.is_empty() {
            return true;
        }
        let elapsed = self.state_entered_at.elapsed().as_secs();
        elapsed >= HEIGHT_QUERY_TIMEOUT_SECS
    }

    /// Compute the median target height from collected responses, set it as the
    /// target, and transition to `Downloading`.
    ///
    /// Returns the computed target height.
    pub fn begin_downloading(&mut self, our_height: u64) -> u64 {
        let raw_target = if self.height_responses.is_empty() {
            // Zero-contact guard (empty-round race): a round that never probed a
            // peer and never heard a response has no evidence the network is at
            // our height — synthesizing `target = our_height` here is what the
            // driver latches as "already close enough — skip to complete",
            // going permanently dormant. Commit target 0 instead: the driver's
            // existing `Downloading`/`target == 0` arm resets and re-queries
            // with fresh probes (or falls back to its solo-node timeout).
            if !self.contacted_this_round {
                warn!(
                    "[sync] height round ended with zero probes and zero responses — refusing to conclude, forcing re-query"
                );
                0
            } else {
                our_height
            }
        } else {
            let mut sorted = self.height_responses.clone();
            sorted.sort_unstable();
            let mid = sorted.len() / 2;
            sorted[mid]
        };

        // Security (see MAX_SYNC_TARGET_GAP): clamp the peer-derived target to a
        // reachable window above our own height so a single peer reporting a
        // bogus height (e.g. u64::MAX) cannot pin us in Downloading forever.
        let target = raw_target.min(our_height.saturating_add(MAX_SYNC_TARGET_GAP));

        info!(
            "[sync] begin_downloading: our_height={} target={} (raw peer target={})",
            our_height, target, raw_target
        );

        self.target_height = target;
        self.height_responses.clear();
        self.state = SyncState::Downloading;
        self.state_entered_at = Instant::now();
        self.current_batch = None;
        // Fresh download attempt: reset the no-progress watchdog baseline.
        self.last_progress_height = our_height;
        self.consecutive_stall_failures = 0;

        target
    }

    /// Return the next batch `(start, end)` to request, or `None` if caught up.
    ///
    /// When caught up (`our_height >= target_height`), transitions to `Verifying`
    /// and clears height responses for re-collection.
    pub fn next_batch(&mut self, our_height: u64) -> Option<(u64, u64)> {
        // QC-024: only a Downloading machine has a meaningful batch. Without this
        // guard the stall watchdog defeats itself INSIDE ONE TICK:
        // `record_batch_failure` calls `reset()` (state=Idle, target_height=0) and
        // the driver then calls `next_batch` a few lines later, where the
        // `our_height >= target_height` branch is trivially true against 0 — so
        // the machine logs "reached target 0 — entering Verifying" and skips the
        // Idle -> start() -> QueryHeight re-query the watchdog exists to trigger.
        // It then verifies with zero evidence and can latch `sync_complete` on a
        // node tens of thousands of blocks behind. Returning None here leaves the
        // machine Idle so the driver's Idle arm restarts it properly next tick.
        if self.state != SyncState::Downloading {
            return None;
        }
        if our_height >= self.target_height {
            info!(
                "[sync] reached target {} — entering Verifying",
                self.target_height
            );
            self.height_responses.clear();
            self.state = SyncState::Verifying;
            self.state_entered_at = Instant::now();
            self.current_batch = None;
            return None;
        }

        // Any forward progress toward the target clears the no-progress watchdog:
        // a genuine (if slow) sync that keeps acquiring blocks never trips it.
        if our_height > self.last_progress_height {
            self.last_progress_height = our_height;
            self.consecutive_stall_failures = 0;
            // QC-024: forward progress ALSO clears the per-peer failure tallies.
            // These were monotonic — only `reset()` cleared them — which was
            // harmless while `batch_timed_out()` could never fire, but becomes a
            // NEW permanent wedge now that it can: a batch that lands between the
            // 5s tick and the 10s timeout is charged a failure AND makes progress
            // in the same tick, so on a long catch-up (~3,000 batches) a peer
            // accumulates MAX_PEER_FAILURES from ordinary slowness alone. Once
            // every peer is exhausted `select_peer` returns None, the node sends
            // no requests and records no failures — so the stall watchdog can
            // never trip either — and it is stuck silently until a restart.
            // A peer that just served us is demonstrably healthy; forget its past.
            self.peer_failures.clear();
            self.exhausted_peers.clear();
        }

        let start = our_height + 1;
        let end = (start + SYNC_BATCH_SIZE - 1).min(self.target_height);
        debug!("[sync] next_batch: ({}, {})", start, end);
        // QC-024: restart the in-flight clock ONLY for a genuinely different
        // batch. This used to reset unconditionally on every call — and the sync
        // driver calls `next_batch` every 5s tick (event_loop.rs sync_timer)
        // while BATCH_TIMEOUT_SECS is 10, so `state_entered_at.elapsed()` could
        // never reach the threshold. `batch_timed_out()` was therefore ALWAYS
        // false, which made `record_batch_failure` (and with it peer exhaustion
        // and the MAX_STALL_BATCH_FAILURES watchdog) unreachable in production:
        // a batch that never returns was re-requested from the same peer
        // forever, with no retry and no escalation. Re-requesting the SAME range
        // must let the clock run.
        let new_batch = Some((start, end));
        if self.current_batch != new_batch {
            self.current_batch = new_batch;
            self.state_entered_at = Instant::now();
        }
        Some((start, end))
    }

    /// Returns `true` if the current batch has been in-flight longer than
    /// `BATCH_TIMEOUT_SECS`.
    pub fn batch_timed_out(&self) -> bool {
        if self.state != SyncState::Downloading {
            return false;
        }
        self.state_entered_at.elapsed().as_secs() >= BATCH_TIMEOUT_SECS
    }

    /// Record a batch failure for `peer`.
    ///
    /// Returns `true` if the peer has now reached `MAX_PEER_FAILURES` and has
    /// been added to the exhausted set.
    ///
    /// Also drives the liveness watchdog (see `MAX_STALL_BATCH_FAILURES`): while
    /// `Downloading` toward a nonzero target, batch failures that occur without
    /// any forward progress accumulate, and once they cross the bound the machine
    /// resets itself back to the initial state so the event loop re-queries fresh
    /// peer heights on the next tick. This is the local liveness fix for the
    /// "bogus target above the true tip but below the clamp" wedge — the reset is
    /// performed internally so it flows through this existing call site with no
    /// change to the (protected) caller and no signature change.
    pub fn record_batch_failure(&mut self, peer: PeerId) -> bool {
        let count = self.peer_failures.entry(peer).or_insert(0);
        *count += 1;
        let newly_exhausted = *count >= MAX_PEER_FAILURES;
        if newly_exhausted {
            warn!(
                "[sync] peer {} exhausted after {} failures",
                peer, count
            );
            self.exhausted_peers.insert(peer);
        }

        // Liveness watchdog: count no-progress batch failures and break the wedge
        // once they cross the bound. Guarded to the Downloading-with-target phase
        // so it never fires spuriously outside an active download.
        if self.state == SyncState::Downloading && self.target_height > 0 {
            self.consecutive_stall_failures = self.consecutive_stall_failures.saturating_add(1);
            if self.consecutive_stall_failures >= MAX_STALL_BATCH_FAILURES {
                warn!(
                    "[sync] no download progress after {} batch failures (stuck at height {}, target {}) — resetting to re-query peer heights",
                    self.consecutive_stall_failures, self.last_progress_height, self.target_height
                );
                self.reset();
            }
        }

        newly_exhausted
    }

    /// Returns `true` when verification data is ready: at least one height
    /// response has been collected, or the verification timeout has elapsed.
    pub fn verification_ready(&self) -> bool {
        if !self.height_responses.is_empty() {
            return true;
        }
        self.state_entered_at.elapsed().as_secs() >= HEIGHT_QUERY_TIMEOUT_SECS
    }

    /// Finalize the verification step.
    ///
    /// If `our_height` has caught up to the median of collected responses (or
    /// responses are empty), transitions to `Complete` and returns `true`.
    ///
    /// Otherwise, updates `target_height` to the new median and transitions back
    /// to `Downloading`, returning `false`.
    pub fn complete_verification(&mut self, our_height: u64) -> bool {
        // Zero-contact guard (empty-round race): never conclude `Complete` from
        // a round with zero probes and zero responses — total silence is not
        // evidence of being caught up. Tear the attempt down to target 0 so the
        // driver's existing `target == 0` arm re-queries with fresh probes.
        if self.height_responses.is_empty() && !self.contacted_this_round {
            warn!(
                "[sync] verification round ended with zero probes and zero responses — refusing to complete, forcing re-query"
            );
            self.target_height = 0;
            self.state = SyncState::Downloading;
            self.state_entered_at = Instant::now();
            self.current_batch = None;
            self.last_progress_height = our_height;
            self.consecutive_stall_failures = 0;
            return false;
        }

        let raw_target = if self.height_responses.is_empty() {
            our_height
        } else {
            let mut sorted = self.height_responses.clone();
            sorted.sort_unstable();
            let mid = sorted.len() / 2;
            sorted[mid]
        };

        // Security (see MAX_SYNC_TARGET_GAP): clamp the peer-derived target so a
        // bogus verification-round height cannot re-poison the target and wedge
        // us back into an unreachable Downloading loop.
        let new_target = raw_target.min(our_height.saturating_add(MAX_SYNC_TARGET_GAP));

        self.height_responses.clear();

        if our_height >= new_target {
            info!("[sync] verification complete at height {}", our_height);
            self.state = SyncState::Complete;
            self.state_entered_at = Instant::now();
            true
        } else {
            info!(
                "[sync] not yet caught up (our={} target={}) — back to Downloading",
                our_height, new_target
            );
            self.target_height = new_target;
            self.state = SyncState::Downloading;
            self.state_entered_at = Instant::now();
            self.current_batch = None;
            // New download window: reset the no-progress watchdog baseline.
            self.last_progress_height = our_height;
            self.consecutive_stall_failures = 0;
            false
        }
    }

    /// Pick a peer from `available` that has not been exhausted.
    ///
    /// Returns `None` if all available peers are exhausted.
    /// Highest height we have observed progress to in the current download.
    /// QC-024: the driver reads this to avoid charging a batch failure in the
    /// same tick that we actually advanced (a late-but-successful batch).
    pub fn last_progress_height(&self) -> u64 {
        self.last_progress_height
    }

    pub fn select_peer(&self, available: &[PeerId]) -> Option<PeerId> {
        for &peer in available {
            if !self.exhausted_peers.contains(&peer) {
                return Some(peer);
            }
        }
        // QC-024: never return None while we still have SOMEONE to ask. Returning
        // None makes the driver send nothing — and because it also records no
        // failure, the stall watchdog can never trip, so the node wedges silently
        // and permanently (only a restart clears `exhausted_peers`). A peer we
        // previously gave up on is strictly better than no peer at all: retrying
        // it can only succeed or re-fail, and re-failing keeps the watchdog alive.
        // Fail OPEN toward liveness, which is the whole point of QC-024.
        available.first().copied()
    }

    /// Re-engagement probe for the (protected) event loop's sync-timer tick.
    ///
    /// The initial sync is one-shot: once the driver latches `sync_complete`
    /// the machine is reset to `Idle` and never consulted again, so a node that
    /// later falls behind a moving tip stays behind forever (2026-07-24
    /// formation wedge: nodes 9–335 blocks below tip for hours). This method is
    /// the standing re-arm check.
    ///
    /// EVENT-LOOP CONTRACT: call once per sync tick — OUTSIDE the
    /// `!sync_complete` gate, so a latched node is still checked — with the
    /// local chain height and the node_state network height. When it returns
    /// `true`, the driver must clear its `sync_complete` latch and `reset()`
    /// this machine, so the next tick's `Idle` arm starts a fresh `QueryHeight`
    /// round.
    ///
    /// Fires only when ALL hold:
    /// - the machine is `Idle` (an active round is left alone);
    /// - `network_height >= local_height + MIN_REENGAGE_LAG`;
    /// - the lag has persisted for `REENGAGE_GRACE_SECS`, measured from first
    ///   observation (the observation resets when the lag breaks, the local
    ///   height changes, or the machine leaves `Idle`);
    /// - the per-fire exponential backoff allows it: the first fire needs only
    ///   the grace, then `REENGAGE_BACKOFF_BASE_SECS` (30s), 60s, … capped at
    ///   `REENGAGE_BACKOFF_CAP_SECS` (300s) between fires. The ladder fully
    ///   resets once the local height advances past the height recorded at the
    ///   last fire.
    ///
    /// A `true` return records the fire (time + local height) for the backoff.
    pub fn should_reengage(&mut self, local_height: u64, network_height: u64) -> bool {
        // Progress past the last fire's height means the fired round worked:
        // fully reset the backoff ladder.
        if let Some((_, fired_height)) = self.reengage_last_fire {
            if local_height > fired_height {
                self.reengage_last_fire = None;
                self.reengage_fires = 0;
            }
        }

        // (i) Only an Idle machine may re-engage; any active state voids the
        // current lag observation.
        if self.state != SyncState::Idle {
            self.reengage_lag_since = None;
            return false;
        }

        // (ii) Minimum lag.
        if network_height < local_height.saturating_add(MIN_REENGAGE_LAG) {
            self.reengage_lag_since = None;
            return false;
        }

        // (iii) Grace: the lag must persist from first observation; the
        // observation restarts whenever the local height changes.
        let now = Instant::now();
        match self.reengage_lag_since {
            Some((since, observed_height)) if observed_height == local_height => {
                if now.duration_since(since).as_secs() < REENGAGE_GRACE_SECS {
                    return false;
                }
            }
            _ => {
                self.reengage_lag_since = Some((now, local_height));
                return false;
            }
        }

        // (iv) Backoff ladder between fires: 30s, 60s, … capped at 300s.
        if let Some((fired_at, _)) = self.reengage_last_fire {
            if now.duration_since(fired_at).as_secs() < self.reengage_backoff_secs() {
                return false;
            }
        }

        info!(
            "[sync] re-engaging: local height {} behind network height {} for >= {}s",
            local_height, network_height, REENGAGE_GRACE_SECS
        );
        self.reengage_last_fire = Some((now, local_height));
        self.reengage_fires = self.reengage_fires.saturating_add(1);
        true
    }

    /// Seconds the backoff ladder requires between the last fire and the next:
    /// `BASE * 2^(fires-1)`, capped at `REENGAGE_BACKOFF_CAP_SECS`. The shift
    /// clamp keeps the shl far below u64 overflow (a wrapped shl could yield a
    /// value UNDER the cap — even 0 — silently voiding the backoff); any shift
    /// >= 4 already exceeds the cap (30 << 4 = 480 > 300), so clamping loses
    /// nothing.
    fn reengage_backoff_secs(&self) -> u64 {
        let shift = self.reengage_fires.saturating_sub(1).min(32);
        (REENGAGE_BACKOFF_BASE_SECS << shift).min(REENGAGE_BACKOFF_CAP_SECS)
    }

    /// Reset the machine back to `Idle`, clearing all transient state.
    ///
    /// Deliberately does NOT clear the re-engagement tracker (lag observation,
    /// fire record, backoff ladder): the driver resets the machine on every
    /// `sync_complete` latch AND on every re-engagement fire, so wiping the
    /// ladder here would let a persistently-lagging node re-fire every grace
    /// period, defeating the backoff.
    pub fn reset(&mut self) {
        info!("[sync] reset to Idle");
        self.state = SyncState::Idle;
        self.target_height = 0;
        self.height_responses.clear();
        self.state_entered_at = Instant::now();
        self.current_batch = None;
        self.peer_failures.clear();
        self.exhausted_peers.clear();
        self.last_progress_height = 0;
        self.consecutive_stall_failures = 0;
        self.contacted_this_round = false;
    }
}

impl Default for SyncMachine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: create a deterministic PeerId for tests.
    fn make_peer() -> PeerId {
        PeerId::random()
    }

    #[test]
    fn starts_idle() {
        let machine = SyncMachine::new();
        assert_eq!(*machine.state(), SyncState::Idle);
    }

    #[test]
    fn start_transitions_to_query_height() {
        let mut machine = SyncMachine::new();
        machine.start();
        assert_eq!(*machine.state(), SyncState::QueryHeight);
    }

    #[test]
    fn downloading_produces_batches() {
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(100);
        let target = machine.begin_downloading(0);
        assert_eq!(target, 100);
        assert_eq!(*machine.state(), SyncState::Downloading);

        // First batch: blocks 1–10
        let batch1 = machine.next_batch(0);
        assert_eq!(batch1, Some((1, 10)));

        // Second batch: blocks 11–20
        let batch2 = machine.next_batch(10);
        assert_eq!(batch2, Some((11, 20)));
    }

    #[test]
    fn downloading_transitions_to_verifying_when_caught_up() {
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(10);
        machine.begin_downloading(0);

        // Consume all batches.
        machine.next_batch(0);  // (1, 10)
        // Now at target.
        let result = machine.next_batch(10);
        assert_eq!(result, None);
        assert_eq!(*machine.state(), SyncState::Verifying);
    }

    #[test]
    fn verification_completes_when_caught_up() {
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(10);
        machine.begin_downloading(0);
        // Drive to Verifying.
        machine.next_batch(10); // already at target → Verifying

        // Collect a height response and verify.
        machine.record_height(10);
        let done = machine.complete_verification(10);
        assert!(done);
        assert_eq!(*machine.state(), SyncState::Complete);
    }

    #[test]
    fn verification_continues_if_behind() {
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(100);
        machine.begin_downloading(0);
        // Simulate having downloaded to 100 and entering Verifying.
        machine.next_batch(100); // → Verifying

        // Peers now report height 200.
        machine.record_height(200);
        let done = machine.complete_verification(100);
        assert!(!done);
        assert_eq!(*machine.state(), SyncState::Downloading);
        assert_eq!(machine.target_height(), 200);
    }

    // ---- Security: target-height clamp (numeric bound ONLY) ----
    //
    // These two tests assert ONLY that the peer-derived target is numerically
    // clamped to `our_height + MAX_SYNC_TARGET_GAP`. They deliberately say
    // NOTHING about whether the node can escape a wedge: the clamp keeps a bogus
    // target reachable but not necessarily true, so on a short chain a target
    // above the real tip yet below the clamp still stalls the download. Liveness
    // out of that stall is covered separately by
    // `watchdog_resets_wedged_downloading_after_stall` below — do not read these
    // clamp tests as evidence the stall is resolved.

    #[test]
    fn begin_downloading_clamps_bogus_peer_height_to_numeric_bound() {
        let mut machine = SyncMachine::new();
        machine.start();
        // A single malicious peer reports u64::MAX (wins the median as sole
        // responder).
        machine.record_height(u64::MAX);
        let target = machine.begin_downloading(0);
        // Target must be clamped to a reachable window, never u64::MAX. (Numeric
        // bound only — says nothing about escaping a below-clamp bogus target.)
        assert_eq!(target, MAX_SYNC_TARGET_GAP);
        assert_eq!(machine.target_height(), MAX_SYNC_TARGET_GAP);
        assert!(machine.target_height() < u64::MAX);
    }

    #[test]
    fn complete_verification_clamps_bogus_peer_height_to_numeric_bound() {
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(10);
        machine.begin_downloading(0);
        machine.next_batch(10); // → Verifying
        // Malicious peer now reports u64::MAX during verification.
        machine.record_height(u64::MAX);
        let done = machine.complete_verification(10);
        // our_height (10) < clamped target (10 + GAP), so not done, but the
        // target is clamped and reachable rather than pinned at u64::MAX.
        // (Numeric bound only — not a liveness claim.)
        assert!(!done);
        assert_eq!(machine.target_height(), 10 + MAX_SYNC_TARGET_GAP);
        assert!(machine.target_height() < u64::MAX);
    }

    // ---- Security: liveness watchdog (escape the below-clamp wedge) ----

    /// NON-VACUOUS: drives the machine into the real wedge — a bogus target ABOVE
    /// the true tip but BELOW the clamp — then downloads the whole real chain and
    /// hammers batch failures with NO further progress, exactly as the event loop
    /// does on repeated batch timeouts. The watchdog must tear the attempt down
    /// (target back to 0, state back to the initial `Idle`) so the next tick
    /// re-queries fresh heights. Against the pre-watchdog code `record_batch_failure`
    /// only ever increments a per-peer counter and `next_batch` keeps returning
    /// `Some` forever (our_height < target), so `target_height()` stays at the
    /// bogus value and the state never leaves `Downloading` — this test fails.
    #[test]
    fn watchdog_resets_wedged_downloading_after_stall() {
        let mut machine = SyncMachine::new();
        machine.start();
        // Bogus peer target: real tip is 500, but a malicious peer reports 5_000
        // (above the tip, comfortably below MAX_SYNC_TARGET_GAP so the clamp does
        // NOT save us).
        let true_tip = 500u64;
        let bogus_target = 5_000u64;
        assert!(bogus_target < MAX_SYNC_TARGET_GAP);
        machine.record_height(bogus_target);
        let target = machine.begin_downloading(0);
        assert_eq!(target, bogus_target, "clamp leaves a below-clamp target intact");
        assert_eq!(*machine.state(), SyncState::Downloading);

        // Download the whole REAL chain up to the true tip — genuine progress each
        // batch (mirrors event_loop calling next_batch as our_height advances).
        let mut our_height = 0u64;
        while our_height < true_tip {
            let batch = machine.next_batch(our_height);
            assert!(batch.is_some(), "still below bogus target → always Some");
            our_height = batch.unwrap().1;
        }
        our_height = true_tip;

        // Now wedged at the true tip: next_batch never returns None (500 < 5000),
        // so Verifying / the target==0 reset are never reached.
        assert!(machine.next_batch(our_height).is_some());
        assert_eq!(*machine.state(), SyncState::Downloading);

        // Every batch for a block past the true tip times out. Feed no-progress
        // failures through the EXISTING call site until the watchdog fires.
        let peer = make_peer();
        let mut reset = false;
        for _ in 0..(MAX_STALL_BATCH_FAILURES as usize + 8) {
            // QC-024: this MUST mirror the driver's real order — the event loop
            // charges the timeout FIRST and then calls next_batch in the same tick
            // (event_loop.rs, Downloading arm). The old test had them reversed,
            // which is precisely why it never caught the watchdog defeating
            // itself: record_batch_failure resets (Idle, target 0), and the
            // following next_batch used to see `our_height >= 0`, log "reached
            // target 0" and jump to Verifying — so the machine skipped the
            // Idle -> start() -> QueryHeight re-query the watchdog exists to
            // deliver, and could then latch sync_complete while far behind.
            machine.record_batch_failure(peer);
            let _ = machine.next_batch(our_height);
            if *machine.state() == SyncState::Idle {
                reset = true;
                break;
            }
        }

        assert!(reset, "watchdog must break the wedge, not stall forever");
        assert_eq!(machine.target_height(), 0, "reset clears the bogus target");
        assert_eq!(*machine.state(), SyncState::Idle, "back to initial state to re-query");
    }

    /// The watchdog must NOT punish a slow-but-genuine sync: as long as batches
    /// keep advancing our height, the no-progress counter resets and the machine
    /// stays in `Downloading` no matter how many total (interleaved) failures
    /// occur. Guards against an over-eager reset breaking legitimate catch-up.
    #[test]
    fn watchdog_tolerates_failures_interleaved_with_progress() {
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(1_000);
        machine.begin_downloading(0);
        let peer = make_peer();

        let mut our_height = 0u64;
        // Many more failures than the threshold, but a real block arrives between
        // each one, so the stall counter never accumulates.
        for _ in 0..(MAX_STALL_BATCH_FAILURES as usize * 3) {
            machine.record_batch_failure(peer);
            let batch = machine.next_batch(our_height).expect("below target");
            our_height = batch.1; // progress
        }
        assert_eq!(
            *machine.state(),
            SyncState::Downloading,
            "progress between failures must keep the sync alive"
        );
        assert_eq!(machine.target_height(), 1_000);
    }

    #[test]
    fn legit_target_within_window_unchanged() {
        // A realistic peer height within the window is used verbatim — the clamp
        // never rejects legitimate catch-up targets.
        let mut machine = SyncMachine::new();
        machine.start();
        machine.record_height(5_000);
        let target = machine.begin_downloading(0);
        assert_eq!(target, 5_000);
    }

    // ---- Re-engagement (2026-07-24 formation wedge) ----
    //
    // Time control: the tracker keys off `Instant`s captured at observation/fire
    // time; tests age them by subtracting from the stored value. `checked_sub`
    // guards the (theoretical) case of a test host whose monotonic clock is
    // younger than the backdate.

    fn backdate_lag(m: &mut SyncMachine, secs: u64) {
        if let Some((t, _)) = m.reengage_lag_since.as_mut() {
            *t = t
                .checked_sub(std::time::Duration::from_secs(secs))
                .expect("test host monotonic clock too young to backdate");
        }
    }

    fn backdate_fire(m: &mut SyncMachine, secs: u64) {
        if let Some((t, _)) = m.reengage_last_fire.as_mut() {
            *t = t
                .checked_sub(std::time::Duration::from_secs(secs))
                .expect("test host monotonic clock too young to backdate");
        }
    }

    #[test]
    fn reengage_fires_after_sustained_lag() {
        let mut m = SyncMachine::new();
        // First qualifying call only arms the grace timer — never fires at once.
        assert!(!m.should_reengage(10, 20));
        // Grace not yet elapsed.
        assert!(!m.should_reengage(10, 20));
        backdate_lag(&mut m, REENGAGE_GRACE_SECS);
        assert!(m.should_reengage(10, 20), "sustained lag past grace must fire");
        // Fire recorded: an immediate repeat is gated by the backoff ladder.
        assert!(!m.should_reengage(10, 20));
    }

    #[test]
    fn reengage_respects_min_lag() {
        let mut m = SyncMachine::new();
        // Lag of 1 (< MIN_REENGAGE_LAG) never arms, never fires.
        assert!(!m.should_reengage(10, 11));
        backdate_lag(&mut m, REENGAGE_GRACE_SECS * 10); // no-op: nothing armed
        assert!(!m.should_reengage(10, 11));
        // Exactly MIN_REENGAGE_LAG arms and (after grace) fires.
        assert!(!m.should_reengage(10, 12));
        backdate_lag(&mut m, REENGAGE_GRACE_SECS);
        assert!(m.should_reengage(10, 12));
    }

    #[test]
    fn reengage_grace_resets_when_lag_clears() {
        let mut m = SyncMachine::new();
        assert!(!m.should_reengage(10, 20)); // arm
        backdate_lag(&mut m, REENGAGE_GRACE_SECS); // ripe: a lagging call would fire
        // Lag clears — the observation must reset.
        assert!(!m.should_reengage(20, 20));
        // Lag returns: this is a FRESH observation; had the ripe one survived
        // the clear, this call would (wrongly) fire.
        assert!(!m.should_reengage(20, 30));
    }

    #[test]
    fn reengage_backoff_grows_then_resets_on_progress() {
        let mut m = SyncMachine::new();
        // Fire #1 needs only the grace.
        assert!(!m.should_reengage(10, 20));
        backdate_lag(&mut m, REENGAGE_GRACE_SECS);
        assert!(m.should_reengage(10, 20));
        // Fire #2 requires BASE (30s) since fire #1.
        backdate_fire(&mut m, REENGAGE_BACKOFF_BASE_SECS - 1);
        assert!(!m.should_reengage(10, 20));
        backdate_fire(&mut m, 1); // 30s total
        assert!(m.should_reengage(10, 20));
        // Fire #3 requires 60s.
        backdate_fire(&mut m, REENGAGE_BACKOFF_BASE_SECS); // only 30s
        assert!(!m.should_reengage(10, 20));
        backdate_fire(&mut m, REENGAGE_BACKOFF_BASE_SECS); // 60s total
        assert!(m.should_reengage(10, 20));
        // The ladder caps at 300s: however many fires accumulate, a 300s wait
        // always suffices...
        for _ in 0..6 {
            backdate_fire(&mut m, REENGAGE_BACKOFF_CAP_SECS);
            assert!(m.should_reengage(10, 20));
        }
        // ...and (one second under the cap) is still not enough.
        backdate_fire(&mut m, REENGAGE_BACKOFF_CAP_SECS - 1);
        assert!(!m.should_reengage(10, 20));
        backdate_fire(&mut m, 1);
        assert!(m.should_reengage(10, 20));
        // Local height advances past the last fire's height: the ladder fully
        // resets — the next fire needs only the grace again, not 300s.
        assert!(!m.should_reengage(11, 20)); // fresh observation at new height
        backdate_lag(&mut m, REENGAGE_GRACE_SECS);
        assert!(m.should_reengage(11, 20), "backoff must reset after progress");
    }

    #[test]
    fn reengage_only_when_idle() {
        let mut m = SyncMachine::new();
        // Arm and ripen the lag while Idle.
        assert!(!m.should_reengage(10, 20));
        backdate_lag(&mut m, REENGAGE_GRACE_SECS);
        // Machine becomes active: must not fire, and the observation is voided.
        m.start();
        assert!(!m.should_reengage(10, 20));
        // Back to Idle: a fresh grace period is required before firing.
        m.reset();
        assert!(!m.should_reengage(10, 20));
        backdate_lag(&mut m, REENGAGE_GRACE_SECS);
        assert!(m.should_reengage(10, 20));
    }

    // ---- Empty-round race: no conclusion without contact ----

    /// A round that never probed a peer and never heard a height response has
    /// zero evidence about the network — it must NOT conclude "nothing to do"
    /// (which the driver latches as sync-complete, going permanently dormant).
    /// Against the pre-fix code, `begin_downloading` on total silence
    /// synthesizes `target = our_height` and `complete_verification` reaches
    /// `Complete` — both latch paths for a node whose probes were all lost.
    #[test]
    fn empty_round_requires_probe() {
        // Zero contact: the query round must force a re-query (target 0 flows
        // into the driver's existing target==0 reset/re-probe path)...
        let mut m = SyncMachine::new();
        m.start();
        let target = m.begin_downloading(7);
        assert_eq!(target, 0, "zero-contact round must re-query, not conclude");
        // ...and even if driven to Verifying, total silence must not Complete.
        m.next_batch(7); // our_height >= target(0) → Verifying
        assert_eq!(*m.state(), SyncState::Verifying);
        assert!(!m.complete_verification(7));
        assert_ne!(*m.state(), SyncState::Complete);

        // With a probe attempt on record, an empty (silent) round MAY conclude:
        // we tried, nobody answered — same behavior as before the guard.
        let mut m = SyncMachine::new();
        m.start();
        m.record_probes_sent(3);
        assert_eq!(m.begin_downloading(7), 7);
        m.next_batch(7); // → Verifying
        assert!(m.complete_verification(7));
        assert_eq!(*m.state(), SyncState::Complete);

        // A peer height response also counts as contact.
        let mut m = SyncMachine::new();
        m.start();
        m.record_height(7);
        assert_eq!(m.begin_downloading(7), 7);
    }
}
