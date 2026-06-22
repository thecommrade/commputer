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
use std::time::{Duration, Instant};

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
}

impl BridgeTransport {
    /// Block indefinitely for each reply (relies on the backend obligation to always reply-or-drop).
    pub fn new(cmd_tx: Sender<DaCommand>) -> Self {
        Self { cmd_tx, call_timeout: None }
    }

    /// Bound each reply wait by `timeout`; on timeout the call returns the unavailable default
    /// (→ facade `Abstain`) so a parked/misbehaving backend cannot hang the caller. Recommended for
    /// production (set generously — at least the DA retry window).
    pub fn with_timeout(cmd_tx: Sender<DaCommand>, timeout: Duration) -> Self {
        Self { cmd_tx, call_timeout: Some(timeout) }
    }

    /// Block on a reply, mapping any error (disconnect or timeout) to `T::default()` — the unavailable
    /// value for every query type (`Vec::new()` / `None` / `false`).
    fn block_on_reply<T: Default>(&self, rx: Receiver<T>) -> T {
        match self.call_timeout {
            Some(d) => rx.recv_timeout(d).unwrap_or_default(),
            None => rx.recv().unwrap_or_default(),
        }
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
