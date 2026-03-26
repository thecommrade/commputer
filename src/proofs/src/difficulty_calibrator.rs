//! Item 151: Proof difficulty auto-calibration.
//!
//! Adjusts difficulty so that the average proof completion time
//! converges toward a target (default ~10 seconds). Uses an
//! exponential moving average of recent proof times.

use commputer_core::proof::ResourceChannel;
use std::collections::HashMap;

/// Target proof completion time in milliseconds.
const TARGET_MS: u64 = 10_000;

/// Maximum difficulty multiplier.
const MAX_DIFFICULTY: f64 = 10.0;
/// Minimum difficulty multiplier.
const MIN_DIFFICULTY: f64 = 0.1;

/// Smoothing factor for exponential moving average (0-1).
/// Lower = smoother / slower to react.
const EMA_ALPHA: f64 = 0.2;

/// Auto-calibrates proof difficulty per channel.
pub struct DifficultyCalibrator {
    /// Current difficulty multiplier per channel.
    difficulties: HashMap<ResourceChannel, f64>,
    /// Exponential moving average of proof times per channel (ms).
    ema_times: HashMap<ResourceChannel, f64>,
    /// Number of samples recorded per channel.
    sample_counts: HashMap<ResourceChannel, u64>,
    /// Target completion time in ms.
    pub target_ms: u64,
}

/// Snapshot of calibration state for one channel.
#[derive(Debug, Clone)]
pub struct CalibrationSnapshot {
    pub channel: ResourceChannel,
    pub current_difficulty: f64,
    pub avg_completion_ms: f64,
    pub target_ms: u64,
    pub samples: u64,
    /// Whether the difficulty needs adjustment.
    pub needs_adjustment: bool,
}

impl DifficultyCalibrator {
    /// Create a new calibrator with default target.
    pub fn new() -> Self {
        Self::with_target(TARGET_MS)
    }

    /// Create a calibrator with a custom target time.
    pub fn with_target(target_ms: u64) -> Self {
        let mut difficulties = HashMap::new();
        for ch in ResourceChannel::ALL {
            difficulties.insert(ch, 1.0);
        }

        Self {
            difficulties,
            ema_times: HashMap::new(),
            sample_counts: HashMap::new(),
            target_ms,
        }
    }

    /// Record a proof completion time for a channel.
    pub fn record_completion(&mut self, channel: ResourceChannel, completion_ms: u64) {
        let count = self.sample_counts.entry(channel).or_insert(0);
        *count += 1;

        let ema = self.ema_times.entry(channel).or_insert(completion_ms as f64);
        *ema = EMA_ALPHA * completion_ms as f64 + (1.0 - EMA_ALPHA) * *ema;
    }

    /// Get the current difficulty multiplier for a channel.
    pub fn get_difficulty(&self, channel: ResourceChannel) -> f64 {
        self.difficulties.get(&channel).copied().unwrap_or(1.0)
    }

    /// Get all current difficulties.
    pub fn get_all_difficulties(&self) -> HashMap<ResourceChannel, f64> {
        self.difficulties.clone()
    }

    /// Recalibrate all channels based on recent completion times.
    /// Call this at the end of each epoch.
    pub fn recalibrate(&mut self) {
        for channel in ResourceChannel::ALL {
            if let Some(&ema_time) = self.ema_times.get(&channel) {
                let samples = self.sample_counts.get(&channel).copied().unwrap_or(0);
                if samples < 3 {
                    continue; // Not enough data to calibrate.
                }

                let current_difficulty = self.difficulties.get(&channel).copied().unwrap_or(1.0);

                // Ratio: if avg time is higher than target, decrease difficulty.
                // If avg time is lower than target, increase difficulty.
                let ratio = self.target_ms as f64 / ema_time;

                // Apply a dampened adjustment (don't overshoot).
                let adjustment = 1.0 + (ratio - 1.0) * 0.5; // 50% dampening
                let new_difficulty = (current_difficulty * adjustment)
                    .clamp(MIN_DIFFICULTY, MAX_DIFFICULTY);

                self.difficulties.insert(channel, new_difficulty);
            }
        }
    }

    /// Get a snapshot of calibration state for a channel.
    pub fn snapshot(&self, channel: ResourceChannel) -> CalibrationSnapshot {
        let current_difficulty = self.get_difficulty(channel);
        let avg_completion_ms = self.ema_times.get(&channel).copied().unwrap_or(0.0);
        let samples = self.sample_counts.get(&channel).copied().unwrap_or(0);

        // Needs adjustment if avg is more than 30% off target.
        let needs_adjustment = if avg_completion_ms > 0.0 {
            let ratio = avg_completion_ms / self.target_ms as f64;
            ratio < 0.7 || ratio > 1.3
        } else {
            false
        };

        CalibrationSnapshot {
            channel,
            current_difficulty,
            avg_completion_ms,
            target_ms: self.target_ms,
            samples,
            needs_adjustment,
        }
    }

    /// Get snapshots for all channels.
    pub fn snapshot_all(&self) -> Vec<CalibrationSnapshot> {
        ResourceChannel::ALL.iter().map(|ch| self.snapshot(*ch)).collect()
    }

    /// Reset calibration for a channel.
    pub fn reset_channel(&mut self, channel: ResourceChannel) {
        self.difficulties.insert(channel, 1.0);
        self.ema_times.remove(&channel);
        self.sample_counts.remove(&channel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_151_initial_difficulty_is_one() {
        let cal = DifficultyCalibrator::new();
        assert_eq!(cal.get_difficulty(ResourceChannel::Processing), 1.0);
        assert_eq!(cal.get_difficulty(ResourceChannel::Gpu), 1.0);
    }

    #[test]
    fn item_151_recalibrate_increases_difficulty_for_fast_proofs() {
        let mut cal = DifficultyCalibrator::new();
        // Proofs completing in 1 second (target is 10s).
        for _ in 0..5 {
            cal.record_completion(ResourceChannel::Processing, 1000);
        }
        cal.recalibrate();
        let d = cal.get_difficulty(ResourceChannel::Processing);
        assert!(d > 1.0, "difficulty should increase for fast proofs: {}", d);
    }

    #[test]
    fn item_151_recalibrate_decreases_difficulty_for_slow_proofs() {
        let mut cal = DifficultyCalibrator::new();
        // Proofs completing in 30 seconds (target is 10s).
        for _ in 0..5 {
            cal.record_completion(ResourceChannel::Gpu, 30_000);
        }
        cal.recalibrate();
        let d = cal.get_difficulty(ResourceChannel::Gpu);
        assert!(d < 1.0, "difficulty should decrease for slow proofs: {}", d);
    }

    #[test]
    fn item_151_difficulty_clamped() {
        let mut cal = DifficultyCalibrator::new();
        // Extremely fast proofs should not push difficulty beyond MAX.
        for _ in 0..10 {
            cal.record_completion(ResourceChannel::Processing, 1);
            cal.recalibrate();
        }
        let d = cal.get_difficulty(ResourceChannel::Processing);
        assert!(d <= MAX_DIFFICULTY);
    }

    #[test]
    fn item_151_snapshot() {
        let mut cal = DifficultyCalibrator::new();
        cal.record_completion(ResourceChannel::Storage, 5000);
        cal.record_completion(ResourceChannel::Storage, 5000);
        cal.record_completion(ResourceChannel::Storage, 5000);

        let snap = cal.snapshot(ResourceChannel::Storage);
        assert_eq!(snap.samples, 3);
        assert!(snap.avg_completion_ms > 0.0);
        assert!(snap.needs_adjustment); // 5s vs 10s target
    }

    #[test]
    fn item_151_custom_target() {
        let cal = DifficultyCalibrator::with_target(5000);
        assert_eq!(cal.target_ms, 5000);
    }

    #[test]
    fn item_151_reset_channel() {
        let mut cal = DifficultyCalibrator::new();
        cal.record_completion(ResourceChannel::Ram, 5000);
        cal.record_completion(ResourceChannel::Ram, 5000);
        cal.record_completion(ResourceChannel::Ram, 5000);
        cal.recalibrate();
        assert_ne!(cal.get_difficulty(ResourceChannel::Ram), 1.0);

        cal.reset_channel(ResourceChannel::Ram);
        assert_eq!(cal.get_difficulty(ResourceChannel::Ram), 1.0);
    }

    #[test]
    fn item_151_not_enough_samples_no_change() {
        let mut cal = DifficultyCalibrator::new();
        cal.record_completion(ResourceChannel::Bandwidth, 1000);
        cal.recalibrate();
        // Only 1 sample, should not change.
        assert_eq!(cal.get_difficulty(ResourceChannel::Bandwidth), 1.0);
    }
}
