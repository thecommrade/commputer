//! Module 9 — the real DA transport bridge + monotonic clock (blueprint phase P4 / G8).
//!
//! Retires the last mocked DA layer in staging. The frozen `DataAvailability` is SYNCHRONOUS but
//! libp2p is async; `BridgeTransport` resolves this without dragging async into staging: each sync
//! `DaTransport` call becomes a `DaCommand` sent over a `std::sync::mpsc` channel, blocking on a
//! reply. The backend (the founder's tokio task driving the node's libp2p swarm, or a test thread)
//! lives entirely behind the channel — so this module is pure `std`, zero new deps, and the frozen
//! `da` crate stays byte-identical (this lives in pouw-onchain). `MonotonicClock` is the real
//! retry-window clock (never hashed into consensus — open-Q#5).
//!
//! Failure contract: a closed/blocked backend → unavailable defaults → facade `Abstain`, never a
//! panic or hang. WIRE-IN: see the P4 founder blueprint for the node-swarm Kademlia + request-response
//! behaviours and the G8 publisher obligation.

use commputer_da::params::ProviderId;
use commputer_da::transport::{Clock, DaTransport, MerklePath};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Default minimum spacing between successive `fetch_chunk` calls on the PRODUCTION construction
/// path (`BridgeTransport::with_timeout`, the exact constructor `main.rs` uses for the real
/// executor/verifier loops — see `da_attestation.rs`). Go-live Task B: the inbound `GetChunk` serve
/// side is rate-limited to 10/s per requesting peer (`network/src/sync_rate_limiter.rs`
/// `MAX_SYNC_REQUESTS_PER_SECOND`, tag-2 bucket). A verifier/executor serially fetching more than
/// 10 sampled chunks in under a second from the sole holder (only the publisher holds chunks today
/// — no replication yet) would get rate-limited into `None` for chunks past the 10th, burning
/// attempts into an early Abstain regardless of peer count. 150ms keeps us comfortably under that
/// limit (≈6.7 req/s) with margin; `SAMPLES_PER_VERIFIER` (16) x 150ms = 2.4s total pacing, which
/// is negligible against both the ≈20s on-chain commit window and the 30s DA retry-window budget
/// (`executor_loop::DEFAULT_DA_RETRY_WINDOW_TICKS`) it must fit inside.
pub const DEFAULT_MIN_FETCH_INTERVAL: Duration = Duration::from_millis(150);

/// Real monotonic clock for the DA retry window (1 tick = 1 ms). NEVER hashed into a consensus value.
pub struct MonotonicClock {
    start: Instant,
}

impl MonotonicClock {
    pub fn new() -> Self {
        Self { start: Instant::now() }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonotonicClock {
    fn now_tick(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

/// A command the sync `BridgeTransport` sends to an async backend (the founder's libp2p swarm task,
/// or a test thread). Each query carries a fresh oneshot reply `Sender`; `Advertise` is fire-and-forget.
///
/// **Backend obligation (the single load-bearing invariant for the bridge's non-hang guarantee):** for
/// every *query* command (`FindProviders`/`FetchChunk`/`HasChunk`) the backend MUST, on EVERY path
/// (success, internal error, shutdown, cancellation), either send exactly one reply OR drop the reply
/// `Sender` — never park while holding it. Dropping the `Sender` makes the bridge's `recv` return
/// `Err` → the unavailable default → facade `Abstain`. (Use an RAII drop-guard around the `Sender` in
/// the production swarm task.) The bridge additionally offers [`BridgeTransport::with_timeout`] as a
/// belt-and-suspenders bound for a misbehaving backend that violates this.
pub enum DaCommand {
    Advertise { chunk_hash: [u8; 32], me: ProviderId },
    FindProviders { chunk_hash: [u8; 32], reply: Sender<Vec<ProviderId>> },
    FetchChunk { chunk_hash: [u8; 32], from: ProviderId, reply: Sender<Option<(Vec<u8>, MerklePath)>> },
    HasChunk { chunk_hash: [u8; 32], reply: Sender<bool> },
}

/// Sync `DaTransport` over an async backend: each method sends a `DaCommand` and blocks on the reply.
/// Pure `std` — async/libp2p lives behind `cmd_tx`. A gone/silent backend (send error, or recv error
/// from a dropped reply `Sender`) yields the unavailable default so the frozen facade degrades to
/// `Abstain` and NEVER panics. With [`Self::with_timeout`] set, a backend that parks while holding the
/// reply `Sender` also degrades to the default after the bound (instead of stalling the verifier) —
/// defense-in-depth for the consensus path.
pub struct BridgeTransport {
    cmd_tx: Sender<DaCommand>,
    /// Per-call upper bound on waiting for a reply. `None` blocks indefinitely (fine for a trusted /
    /// test backend that always replies-or-drops); production should set a generous bound.
    call_timeout: Option<Duration>,
    /// Go-live Task B — client-side pacing: the minimum spacing enforced between the START of
    /// successive `fetch_chunk` sends. `None` (or a zero `Duration`) disables pacing entirely (the
    /// `new()` test constructor's default, so existing suites stay fast). `Some(d)` sleeps just
    /// long enough since the last `fetch_chunk` call before sending the next one — see
    /// [`Self::with_min_fetch_interval`] and [`DEFAULT_MIN_FETCH_INTERVAL`]. Runs on dedicated OS
    /// threads only (never the event loop), so blocking-sleep here is safe.
    min_fetch_interval: Option<Duration>,
    /// When the last `fetch_chunk` call was SENT, for pacing. `Mutex` because `DaTransport` methods
    /// take `&self` (the trait is shared across the executor/verifier loop's call sites).
    last_fetch_at: Mutex<Option<Instant>>,
}

impl BridgeTransport {
    /// Block indefinitely for each reply (relies on the backend obligation to always reply-or-drop).
    /// No fetch pacing by default — this is the constructor every existing test uses, and suites
    /// must stay fast; opt in explicitly via [`Self::with_min_fetch_interval`].
    pub fn new(cmd_tx: Sender<DaCommand>) -> Self {
        Self {
            cmd_tx,
            call_timeout: None,
            min_fetch_interval: None,
            last_fetch_at: Mutex::new(None),
        }
    }

    /// Bound each reply wait by `timeout`; on timeout the call returns the unavailable default
    /// (→ facade `Abstain`) so a parked/misbehaving backend cannot hang the caller. Recommended for
    /// production (set generously — at least the DA retry window). This IS the exact constructor
    /// `main.rs` calls for the real executor/verifier loops (`da_attestation.rs` wraps whatever it
    /// receives generically, so the pacing default lives HERE rather than requiring a protected
    /// `main.rs` edit): defaults `min_fetch_interval` to [`DEFAULT_MIN_FETCH_INTERVAL`] (150ms).
    /// Override with [`Self::with_min_fetch_interval`] if a caller needs it off.
    pub fn with_timeout(cmd_tx: Sender<DaCommand>, timeout: Duration) -> Self {
        Self {
            cmd_tx,
            call_timeout: Some(timeout),
            min_fetch_interval: Some(DEFAULT_MIN_FETCH_INTERVAL),
            last_fetch_at: Mutex::new(None),
        }
    }

    /// Builder: set (or clear, with `Duration::ZERO`) the minimum spacing between successive
    /// `fetch_chunk` calls. See the [`Self`] field doc + [`DEFAULT_MIN_FETCH_INTERVAL`] for why.
    pub fn with_min_fetch_interval(mut self, interval: Duration) -> Self {
        self.min_fetch_interval = if interval.is_zero() { None } else { Some(interval) };
        self
    }

    /// Block on a reply, mapping any error (disconnect or timeout) to `T::default()` — the unavailable
    /// value for every query type (`Vec::new()` / `None` / `false`).
    fn block_on_reply<T: Default>(&self, rx: Receiver<T>) -> T {
        match self.call_timeout {
            Some(d) => rx.recv_timeout(d).unwrap_or_default(),
            None => rx.recv().unwrap_or_default(),
        }
    }

    /// Sleep just enough since the last `fetch_chunk` call to satisfy `min_fetch_interval`, then
    /// record this call's start. A no-op when pacing is unconfigured (the common test path). Uses a
    /// monotonic `Instant` — never hashed into consensus (mirrors `MonotonicClock`'s contract).
    fn pace_fetch(&self) {
        let Some(min_interval) = self.min_fetch_interval else { return };
        let mut last = self.last_fetch_at.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < min_interval {
                std::thread::sleep(min_interval - elapsed);
            }
        }
        *last = Some(Instant::now());
    }
}

impl DaTransport for BridgeTransport {
    fn advertise(&self, chunk_hash: [u8; 32], me: ProviderId) {
        let _ = self.cmd_tx.send(DaCommand::Advertise { chunk_hash, me }); // fire-and-forget
    }

    fn find_providers(&self, chunk_hash: [u8; 32]) -> Vec<ProviderId> {
        let (tx, rx) = channel();
        if self.cmd_tx.send(DaCommand::FindProviders { chunk_hash, reply: tx }).is_err() {
            return Vec::new();
        }
        self.block_on_reply(rx)
    }

    fn fetch_chunk(&self, chunk_hash: [u8; 32], from: ProviderId) -> Option<(Vec<u8>, MerklePath)> {
        self.pace_fetch(); // go-live Task B: stay under the server's 10/s/peer GetChunk serve limit
        let (tx, rx) = channel();
        if self.cmd_tx.send(DaCommand::FetchChunk { chunk_hash, from, reply: tx }).is_err() {
            return None;
        }
        self.block_on_reply(rx)
    }

    fn has_chunk(&self, chunk_hash: [u8; 32]) -> bool {
        let (tx, rx) = channel();
        if self.cmd_tx.send(DaCommand::HasChunk { chunk_hash, reply: tx }).is_err() {
            return false;
        }
        self.block_on_reply(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_clock_is_non_decreasing() {
        let c = MonotonicClock::new();
        let a = c.now_tick();
        let b = c.now_tick();
        assert!(b >= a, "now_tick must be monotonic non-decreasing");
    }

    use std::collections::HashMap;
    use std::sync::mpsc::Receiver;
    use std::thread::JoinHandle;

    type ChunkStore = HashMap<[u8; 32], (ProviderId, Vec<u8>, MerklePath)>;

    /// Spawn an in-memory backend thread servicing DaCommands from `store`. Loop breaks on RecvError
    /// (when the bridge's Sender drops), so callers drop the BridgeTransport then join(). Always
    /// replies on query commands.
    fn spawn_backend(store: ChunkStore) -> (BridgeTransport, JoinHandle<()>) {
        let (tx, rx): (Sender<DaCommand>, Receiver<DaCommand>) = channel();
        let handle = std::thread::spawn(move || {
            let mut store = store;
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    DaCommand::Advertise { chunk_hash, me } => {
                        // record the advertised provider (empty bytes/path is fine for find-only)
                        store.entry(chunk_hash).or_insert((me, Vec::new(), Vec::new()));
                    }
                    DaCommand::FindProviders { chunk_hash, reply } => {
                        let provs = store.get(&chunk_hash).map(|(p, _, _)| vec![*p]).unwrap_or_default();
                        let _ = reply.send(provs);
                    }
                    DaCommand::FetchChunk { chunk_hash, from: _, reply } => {
                        let got = store.get(&chunk_hash).map(|(_, b, p)| (b.clone(), p.clone()));
                        let _ = reply.send(got);
                    }
                    DaCommand::HasChunk { chunk_hash, reply } => {
                        let _ = reply.send(store.contains_key(&chunk_hash));
                    }
                }
            }
        });
        (BridgeTransport::new(tx), handle)
    }

    #[test]
    fn bridge_round_trips_against_backend() {
        let chunk = [1u8; 32];
        let prov = ProviderId([7; 32]);
        let bytes = vec![9u8, 8, 7];
        let path: MerklePath = vec![Some([2u8; 32]), None];
        let mut store: ChunkStore = HashMap::new();
        store.insert(chunk, (prov, bytes.clone(), path.clone()));
        let (bridge, handle) = spawn_backend(store);

        // find / fetch / has on a stored chunk
        assert_eq!(bridge.find_providers(chunk), vec![prov]);
        assert_eq!(bridge.fetch_chunk(chunk, prov), Some((bytes, path)));
        assert!(bridge.has_chunk(chunk));
        // absent chunk
        assert!(bridge.find_providers([0xFF; 32]).is_empty());
        assert_eq!(bridge.fetch_chunk([0xFF; 32], prov), None);
        assert!(!bridge.has_chunk([0xFF; 32]));
        // advertise a new chunk → it becomes findable
        let newc = [3u8; 32];
        let newp = ProviderId([4; 32]);
        bridge.advertise(newc, newp);
        // advertise is fire-and-forget + async; the next find round-trips through the same queue,
        // so by the time find_providers returns, the advertise has been processed (FIFO single
        // consumer thread).
        assert_eq!(bridge.find_providers(newc), vec![newp]);

        drop(bridge);
        handle.join().unwrap();
    }

    use commputer_da::commit::{build_attestation, chunk_proof};
    use commputer_da::facade::{chunk_hash, AvailabilityOutcome, DataAvailability};
    use commputer_da::params::ChunkingParams;

    /// A deterministic ~2 KB program to chunk.
    fn program_bytes() -> Vec<u8> {
        (0..500u32).flat_map(|i| i.to_le_bytes()).collect()
    }

    #[test]
    fn verify_available_through_bridge_reconstructs() {
        let program = program_bytes();
        let (att, coded) = build_attestation(&program, &ChunkingParams::default()).expect("attestation");
        let prov = ProviderId([200; 32]);
        let mut store: ChunkStore = HashMap::new();
        for i in 0..att.n_total {
            store.insert(chunk_hash(&att, i), (prov, coded[i as usize].clone(), chunk_proof(&coded, i)));
        }
        let (bridge, handle) = spawn_backend(store);
        {
            let clock = MonotonicClock::new();
            let da = DataAvailability {
                transport: &bridge,
                clock: &clock,
                retry_window_ticks: 60_000, // generous (ms) so real wall-clock never times out
                max_attempts_per_chunk: 8,
            };
            let outcome = da.verify_available(&att, [1u8; 32], 1, [10u8; 32]);
            assert_eq!(outcome, AvailabilityOutcome::Available(program), "DA round-trip over the bridge");
        } // da (which borrows &bridge) dropped here
        drop(bridge);
        handle.join().unwrap();
    }

    #[test]
    fn missing_chunks_yield_abstain() {
        let program = program_bytes();
        let (att, _coded) = build_attestation(&program, &ChunkingParams::default()).expect("attestation");
        // backend has NO chunks → every sampled fetch fails → Abstain
        let (bridge, handle) = spawn_backend(HashMap::new());
        {
            let clock = MonotonicClock::new();
            let da = DataAvailability {
                transport: &bridge,
                clock: &clock,
                retry_window_ticks: 60_000,
                max_attempts_per_chunk: 8,
            };
            assert_eq!(da.verify_available(&att, [1u8; 32], 1, [10u8; 32]), AvailabilityOutcome::Abstain);
        }
        drop(bridge);
        handle.join().unwrap();
    }

    #[test]
    fn backend_gone_send_fails_to_unavailable_defaults() {
        // No backend: drop the Receiver, so every send() errors → the bridge returns the defaults.
        let (tx, rx) = channel::<DaCommand>();
        drop(rx);
        let bridge = BridgeTransport::new(tx);
        assert!(bridge.find_providers([1u8; 32]).is_empty());
        assert_eq!(bridge.fetch_chunk([1u8; 32], ProviderId([0; 32])), None);
        assert!(!bridge.has_chunk([1u8; 32]));
        bridge.advertise([1u8; 32], ProviderId([0; 32])); // fire-and-forget: no panic on a closed channel
    }

    #[test]
    fn backend_drops_reply_recv_fails_to_default() {
        // A backend that services HasChunk but DROPS the FetchChunk reply Sender without sending
        // (models a mid-command failure) → the bridge's recv() errors → None, no panic, no hang.
        let (tx, rx): (Sender<DaCommand>, Receiver<DaCommand>) = channel();
        let handle = std::thread::spawn(move || {
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    DaCommand::FetchChunk { reply, .. } => drop(reply), // never sends
                    DaCommand::HasChunk { reply, .. } => { let _ = reply.send(true); }
                    DaCommand::FindProviders { reply, .. } => { let _ = reply.send(Vec::new()); }
                    DaCommand::Advertise { .. } => {}
                }
            }
        });
        let bridge = BridgeTransport::new(tx);
        assert_eq!(bridge.fetch_chunk([1u8; 32], ProviderId([0; 32])), None, "dropped reply → None");
        assert!(bridge.has_chunk([1u8; 32]), "other commands still work");
        drop(bridge);
        handle.join().unwrap();
    }

    #[test]
    fn parked_backend_with_timeout_degrades_to_default() {
        // A misbehaving backend that HOLDS the FetchChunk reply Sender and never replies (parks). A
        // with_timeout bridge must return the default after the bound rather than hanging the caller.
        let (tx, rx): (Sender<DaCommand>, Receiver<DaCommand>) = channel();
        let handle = std::thread::spawn(move || {
            let mut parked = Vec::new(); // hold reply Senders, never send
            while let Ok(cmd) = rx.recv() {
                if let DaCommand::FetchChunk { reply, .. } = cmd {
                    parked.push(reply);
                }
            }
            drop(parked);
        });
        let bridge = BridgeTransport::with_timeout(tx, Duration::from_millis(50));
        let t0 = Instant::now();
        assert_eq!(bridge.fetch_chunk([1u8; 32], ProviderId([0; 32])), None, "parked backend → timeout → None");
        assert!(t0.elapsed() < Duration::from_secs(5), "returned promptly via timeout, did not hang");
        drop(bridge);
        handle.join().unwrap();
    }

    /// Go-live Task B: a `min_fetch_interval` set via `with_min_fetch_interval` spaces successive
    /// `fetch_chunk` calls apart by at least the configured interval (client-side pacing to stay
    /// under the server's 10/s/peer `GetChunk` serve limit — `sync_rate_limiter` tag-2). The mock
    /// backend records the arrival `Instant` of each `FetchChunk` command it services (channel
    /// send->recv latency is sub-microsecond, so "arrival" is effectively "send") and we assert on
    /// the spacing between those two recorded arrivals directly. This avoids a subtly wrong
    /// alternative — timing the caller's wall-clock window around each ROUND TRIP and subtracting —
    /// whose result is `min_fetch_interval - RT1 + RT2` and can dip below the interval whenever the
    /// second round trip happens to be even slightly faster than the first (observed flaky in
    /// practice under parallel `cargo test` load).
    #[test]
    fn min_fetch_interval_paces_successive_fetch_chunk_calls() {
        let chunk = [6u8; 32];
        let prov = ProviderId([1; 32]);
        let bytes = vec![1u8, 2, 3];
        let path: MerklePath = vec![None];

        let arrivals: std::sync::Arc<Mutex<Vec<Instant>>> = std::sync::Arc::new(Mutex::new(Vec::new()));
        let arrivals_bg = arrivals.clone();
        let (tx, rx): (Sender<DaCommand>, Receiver<DaCommand>) = channel();
        let handle = std::thread::spawn(move || {
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    DaCommand::FetchChunk { chunk_hash, reply, .. } => {
                        arrivals_bg.lock().unwrap().push(Instant::now());
                        let got = if chunk_hash == chunk {
                            Some((bytes.clone(), path.clone()))
                        } else {
                            None
                        };
                        let _ = reply.send(got);
                    }
                    DaCommand::FindProviders { reply, .. } => { let _ = reply.send(vec![prov]); }
                    DaCommand::HasChunk { reply, .. } => { let _ = reply.send(true); }
                    DaCommand::Advertise { .. } => {}
                }
            }
        });
        let interval = Duration::from_millis(80);
        let bridge = BridgeTransport::new(tx).with_min_fetch_interval(interval);

        assert!(bridge.fetch_chunk(chunk, prov).is_some(), "first call unpaced, still succeeds");
        assert!(bridge.fetch_chunk(chunk, prov).is_some(), "second call paced, still succeeds");

        drop(bridge);
        handle.join().unwrap();

        let arrivals = arrivals.lock().unwrap();
        assert_eq!(arrivals.len(), 2, "backend must have serviced exactly two FetchChunk commands");
        let spacing = arrivals[1].duration_since(arrivals[0]);
        // A few ms of scheduling-jitter tolerance: `spacing` is the gap between two Instants read on
        // the INDEPENDENT backend thread, not the client's own sleep clock, so cross-thread wake-up
        // latency (observed up to ~0.1ms under heavy parallel `cargo test` load) can shave a hair off
        // an otherwise-correct pacing sleep. The tolerance is tiny relative to `interval` and utterly
        // swamped by the ~unpaced-vs-paced gap this test actually discriminates (near-zero vs 80ms).
        let tolerance = Duration::from_millis(5);
        assert!(
            spacing + tolerance >= interval,
            "successive fetch_chunk sends must be spaced by ~>= min_fetch_interval ({interval:?}), \
             only {spacing:?} apart"
        );
    }

    /// A transport with NO `min_fetch_interval` configured (the plain `BridgeTransport::new` used
    /// throughout this test suite) enforces no spacing at all — suites stay fast.
    #[test]
    fn no_min_fetch_interval_means_no_pacing() {
        let chunk = [7u8; 32];
        let prov = ProviderId([2; 32]);
        let bytes = vec![4u8, 5, 6];
        let path: MerklePath = vec![None];
        let mut store: ChunkStore = HashMap::new();
        store.insert(chunk, (prov, bytes, path));
        let (bridge, handle) = spawn_backend(store); // BridgeTransport::new — no pacing by default

        let t0 = Instant::now();
        assert!(bridge.fetch_chunk(chunk, prov).is_some());
        assert!(bridge.fetch_chunk(chunk, prov).is_some());
        assert!(bridge.fetch_chunk(chunk, prov).is_some());
        assert!(
            t0.elapsed() < Duration::from_millis(50),
            "no min_fetch_interval configured -> three back-to-back fetch_chunk calls must not be \
             delayed, took {:?}",
            t0.elapsed()
        );

        drop(bridge);
        handle.join().unwrap();
    }

    /// `BridgeTransport::with_timeout` (the exact production constructor `main.rs` uses for the
    /// real executor/verifier loops) defaults pacing ON at 150ms, WITHOUT any caller having to ask
    /// for it — this is the "apply the default in `with_timeout`" seam go-live Task B uses instead
    /// of touching protected `main.rs`.
    #[test]
    fn with_timeout_defaults_to_150ms_pacing() {
        let chunk = [8u8; 32];
        let prov = ProviderId([3; 32]);
        let bytes = vec![7u8, 8, 9];
        let path: MerklePath = vec![None];
        let mut store: ChunkStore = HashMap::new();
        store.insert(chunk, (prov, bytes, path));

        let (tx, rx): (Sender<DaCommand>, Receiver<DaCommand>) = channel();
        let handle = std::thread::spawn(move || {
            let mut store = store;
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    DaCommand::FetchChunk { chunk_hash, reply, .. } => {
                        let got = store.get(&chunk_hash).map(|(_, b, p)| (b.clone(), p.clone()));
                        let _ = reply.send(got);
                    }
                    DaCommand::FindProviders { chunk_hash, reply } => {
                        let provs = store.get(&chunk_hash).map(|(p, _, _)| vec![*p]).unwrap_or_default();
                        let _ = reply.send(provs);
                    }
                    DaCommand::HasChunk { chunk_hash, reply } => {
                        let _ = reply.send(store.contains_key(&chunk_hash));
                    }
                    DaCommand::Advertise { chunk_hash, me } => {
                        store.entry(chunk_hash).or_insert((me, Vec::new(), Vec::new()));
                    }
                }
            }
        });
        let bridge = BridgeTransport::with_timeout(tx, Duration::from_secs(5));

        let t0 = Instant::now();
        assert!(bridge.fetch_chunk(chunk, prov).is_some());
        assert!(bridge.fetch_chunk(chunk, prov).is_some());
        assert!(
            t0.elapsed() >= Duration::from_millis(150),
            "with_timeout's default pacing must space two fetch_chunk calls by >= 150ms, only took {:?}",
            t0.elapsed()
        );

        drop(bridge);
        handle.join().unwrap();
    }

    #[test]
    fn dead_backend_verify_available_abstains() {
        // The composed consensus-path failure: a dead backend (Receiver dropped) drives the FROZEN
        // verify_available straight to Abstain — never a panic or hang.
        let program = program_bytes();
        let (att, _coded) = build_attestation(&program, &ChunkingParams::default()).expect("attestation");
        let (tx, rx) = channel::<DaCommand>();
        drop(rx); // no backend
        let bridge = BridgeTransport::new(tx);
        let clock = MonotonicClock::new();
        let da = DataAvailability {
            transport: &bridge,
            clock: &clock,
            retry_window_ticks: 60_000,
            max_attempts_per_chunk: 8,
        };
        assert_eq!(da.verify_available(&att, [1u8; 32], 1, [10u8; 32]), AvailabilityOutcome::Abstain);
    }
}
