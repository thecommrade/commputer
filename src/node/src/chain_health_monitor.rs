// chain_health_monitor.rs — Detect unhealthy chain state
//
// WHAT IT DOES:
//   Monitors the health of the chain and reports issues:
//   - Stuck height: no new block for 30+ seconds
//   - High timeout rate: >50% timeout-finalize in last 20 blocks
//   - Clock drift: track timestamp deltas between consecutive blocks
//   - Peer health: track which peers are voting vs silent
//   - Exposes: ChainHealth { is_healthy, stuck_seconds, timeout_rate, avg_block_time, active_voters }
//
// WHERE IT SHOULD GO: src/node/src/chain_health_monitor.rs
//
// WIRING REQUIRED:
//   1. Add `pub mod chain_health_monitor;` to src/node/src/lib.rs
//   2. Call monitor.record_block() on each finalized block
//   3. Call monitor.record_vote(voter_address) for each consensus vote received
//   4. Call monitor.health() for RPC /health endpoint
//   5. Pass to event_loop.rs for periodic health checks

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tracing::warn;

/// Minimum interval between repeated `chain_health: stuck for ...` warnings.
/// Without this, every snapshot of `health()` (called from the RPC status
/// update path on each tick) would emit an identical warning, producing
/// a log flood once the chain entered a stuck state.
const STUCK_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Block has been stuck for more than this long → unhealthy.
pub const STUCK_THRESHOLD: Duration = Duration::from_secs(30);

/// Track this many recent blocks for timeout rate calculation.
pub const TIMEOUT_WINDOW: usize = 20;

/// Timeout rate above this fraction is considered unhealthy.
pub const TIMEOUT_RATE_THRESHOLD: f64 = 0.5;

/// Silence a voter after this many seconds with no votes.
pub const VOTER_SILENCE_THRESHOLD: Duration = Duration::from_secs(60);

/// How a block was finalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeMethod {
    /// Finalized via Snowball consensus (normal).
    Snowball,
    /// Finalized via timeout (primary didn't respond, view change).
    Timeout,
}

/// A finalized block record for health tracking.
#[derive(Debug, Clone)]
struct BlockRecord {
    height: u64,
    timestamp: u64,     // block timestamp from header
    finalized_at: Instant, // when WE finalized it
    method: FinalizeMethod,
}

/// Health status of the chain.
#[derive(Debug, Clone)]
pub struct ChainHealth {
    /// True if the chain appears healthy.
    pub is_healthy: bool,
    /// How many seconds since the last block (0 if recent).
    pub stuck_seconds: u64,
    /// Fraction of recent blocks finalized by timeout (0.0–1.0).
    pub timeout_rate: f64,
    /// Average block time in seconds (based on recent blocks).
    pub avg_block_time: f64,
    /// Number of validators who voted recently.
    pub active_voters: usize,
    /// Total validators tracked.
    pub total_voters: usize,
    /// Current chain height.
    pub height: u64,
    /// Reasons why the chain is considered unhealthy.
    pub issues: Vec<String>,
}

/// Monitor chain health continuously.
pub struct ChainHealthMonitor {
    recent_blocks: VecDeque<BlockRecord>,
    last_block_time: Instant,
    last_height: u64,
    /// voter_address_hash → last time they voted
    voter_activity: HashMap<u64, Instant>,
    /// Last time we emitted a `chain_health: stuck for ...` warning. `Cell` so
    /// `health()` can throttle through `&self` without mutating the public API.
    last_stuck_warn: Cell<Option<Instant>>,
}

impl ChainHealthMonitor {
    pub fn new() -> Self {
        Self {
            recent_blocks: VecDeque::new(),
            last_block_time: Instant::now(),
            last_height: 0,
            voter_activity: HashMap::new(),
            last_stuck_warn: Cell::new(None),
        }
    }

    /// Record a finalized block.
    pub fn record_block(&mut self, height: u64, timestamp: u64, method: FinalizeMethod) {
        let record = BlockRecord {
            height,
            timestamp,
            finalized_at: Instant::now(),
            method,
        };

        // Evict old records
        while self.recent_blocks.len() >= TIMEOUT_WINDOW {
            self.recent_blocks.pop_front();
        }

        self.recent_blocks.push_back(record);
        self.last_block_time = Instant::now();
        self.last_height = height;
    }

    /// Record a vote received from a validator.
    pub fn record_vote(&mut self, voter_hash: u64) {
        self.voter_activity.insert(voter_hash, Instant::now());
    }

    /// Compute current chain health.
    pub fn health(&self) -> ChainHealth {
        let mut issues = Vec::new();

        // Stuck height check
        let stuck_seconds = self.last_block_time.elapsed().as_secs();
        if stuck_seconds >= STUCK_THRESHOLD.as_secs() {
            issues.push(format!(
                "chain stuck: no new block for {}s (threshold: {}s)",
                stuck_seconds, STUCK_THRESHOLD.as_secs()
            ));
            // Throttle: only emit a stuck warning once per STUCK_WARN_INTERVAL.
            // The previous unconditional warn produced a flood because health()
            // is invoked once per RPC status update (multiple times per second).
            let now = Instant::now();
            let should_warn = match self.last_stuck_warn.get() {
                None => true,
                Some(prev) => now.duration_since(prev) >= STUCK_WARN_INTERVAL,
            };
            if should_warn {
                warn!("chain_health: stuck for {}s", stuck_seconds);
                self.last_stuck_warn.set(Some(now));
            }
        } else {
            // Chain has recovered — reset the throttle so the next stuck
            // episode warns immediately rather than waiting out the interval.
            self.last_stuck_warn.set(None);
        }

        // Timeout rate check
        let timeout_rate = self.timeout_rate();
        if timeout_rate > TIMEOUT_RATE_THRESHOLD {
            issues.push(format!(
                "high timeout rate: {:.1}% (threshold: {}%)",
                timeout_rate * 100.0,
                (TIMEOUT_RATE_THRESHOLD * 100.0) as u32
            ));
        }

        // Average block time
        let avg_block_time = self.avg_block_time();

        // Clock drift check: if avg block time is way off from expected 2s
        if avg_block_time > 10.0 {
            issues.push(format!(
                "slow block time: avg {:.1}s (expected ~2s)",
                avg_block_time
            ));
        }

        // Active voters
        let now = Instant::now();
        let active_voters = self.voter_activity.values()
            .filter(|&&last| now.duration_since(last) < VOTER_SILENCE_THRESHOLD)
            .count();
        let total_voters = self.voter_activity.len();

        ChainHealth {
            is_healthy: issues.is_empty(),
            stuck_seconds,
            timeout_rate,
            avg_block_time,
            active_voters,
            total_voters,
            height: self.last_height,
            issues,
        }
    }

    /// Fraction of recent blocks finalized by timeout.
    fn timeout_rate(&self) -> f64 {
        if self.recent_blocks.is_empty() {
            return 0.0;
        }
        let timeouts = self.recent_blocks.iter()
            .filter(|r| r.method == FinalizeMethod::Timeout)
            .count();
        timeouts as f64 / self.recent_blocks.len() as f64
    }

    /// Average block time based on recent block header timestamps.
    fn avg_block_time(&self) -> f64 {
        if self.recent_blocks.len() < 2 {
            return 2.0; // default expected
        }
        let first = self.recent_blocks.front().unwrap().timestamp;
        let last = self.recent_blocks.back().unwrap().timestamp;
        let span = last.saturating_sub(first);
        let count = (self.recent_blocks.len() - 1) as f64;
        if count == 0.0 { return 2.0; }
        span as f64 / count
    }

    /// Returns true if the chain is healthy.
    pub fn is_healthy(&self) -> bool {
        self.health().is_healthy
    }
}

impl Default for ChainHealthMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn healthy_at_start() {
        let monitor = ChainHealthMonitor::new();
        // Freshly created: last_block_time is now, so 0 stuck seconds
        // (might be 0 or 1 second in slow environments but should be healthy)
        let h = monitor.health();
        assert_eq!(h.timeout_rate, 0.0);
    }

    #[test]
    fn snowball_finalization_healthy() {
        let mut monitor = ChainHealthMonitor::new();
        for i in 0..10 {
            monitor.record_block(i, 1_700_000_000 + i * 2, FinalizeMethod::Snowball);
        }
        let h = monitor.health();
        assert_eq!(h.timeout_rate, 0.0);
        assert!(h.issues.is_empty() || !h.issues.iter().any(|s| s.contains("timeout")));
    }

    #[test]
    fn timeout_rate_calculation() {
        let mut monitor = ChainHealthMonitor::new();
        // 10 snowball + 10 timeout = 50% timeout rate
        for i in 0..10 {
            monitor.record_block(i, 1_700_000_000 + i * 2, FinalizeMethod::Snowball);
        }
        for i in 10..20 {
            monitor.record_block(i, 1_700_000_000 + i * 2, FinalizeMethod::Timeout);
        }
        let h = monitor.health();
        // 20 blocks: exactly 50% timeout
        assert!((h.timeout_rate - 0.5).abs() < 0.01);
    }

    #[test]
    fn high_timeout_rate_unhealthy() {
        let mut monitor = ChainHealthMonitor::new();
        // 15 timeouts out of 20 = 75%
        for i in 0..5 {
            monitor.record_block(i, 1_700_000_000 + i * 2, FinalizeMethod::Snowball);
        }
        for i in 5..20 {
            monitor.record_block(i, 1_700_000_000 + i * 2, FinalizeMethod::Timeout);
        }
        let h = monitor.health();
        assert!(h.timeout_rate > TIMEOUT_RATE_THRESHOLD);
        assert!(!h.issues.is_empty());
    }

    #[test]
    fn active_voters_counted() {
        let mut monitor = ChainHealthMonitor::new();
        monitor.record_vote(1);
        monitor.record_vote(2);
        monitor.record_vote(3);

        let h = monitor.health();
        assert_eq!(h.active_voters, 3);
        assert_eq!(h.total_voters, 3);
    }

    #[test]
    fn avg_block_time_calculated() {
        let mut monitor = ChainHealthMonitor::new();
        // 10 blocks, 2 seconds apart
        for i in 0..10u64 {
            monitor.record_block(i, 1_700_000_000 + i * 2, FinalizeMethod::Snowball);
        }
        let h = monitor.health();
        assert!((h.avg_block_time - 2.0).abs() < 0.1,
            "avg block time should be ~2s, got {}", h.avg_block_time);
    }

    #[test]
    fn rolling_window_evicts_old_blocks() {
        let mut monitor = ChainHealthMonitor::new();
        // First 20 blocks all timeout
        for i in 0..TIMEOUT_WINDOW {
            monitor.record_block(i as u64, 1_700_000_000 + i as u64 * 2, FinalizeMethod::Timeout);
        }
        // Next 20 blocks all snowball → should evict the timeout blocks
        for i in TIMEOUT_WINDOW..TIMEOUT_WINDOW * 2 {
            monitor.record_block(i as u64, 1_700_000_000 + i as u64 * 2, FinalizeMethod::Snowball);
        }
        let h = monitor.health();
        assert_eq!(h.timeout_rate, 0.0,
            "all timeout blocks should be evicted from window");
    }
}
