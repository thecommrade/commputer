//! Item 149: Cross-channel correlation analysis.
//!
//! Detects validators that game one channel by sacrificing another.
//! For example, a validator with perfect GPU scores but zero RAM scores
//! is suspicious — real hardware should contribute across channels.

use commputer_core::identity::Address;
use commputer_core::proof::EpochProofSummary;
use std::collections::HashMap;

/// Flags indicating suspicious cross-channel patterns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuspiciousPattern {
    /// One channel has max score while others have zero.
    SingleChannelDominance { channel: &'static str, score: u32 },
    /// Scores are extremely unbalanced (variance too high).
    HighVariance { variance: u64 },
    /// Sudden change in channel scores between epochs.
    ScoreVolatility { channel: &'static str, delta: i64 },
    /// Impossible hardware profile (e.g., high GPU but zero CPU).
    ImpossibleProfile { reason: String },
}

/// Result of cross-channel analysis for a validator.
#[derive(Debug, Clone)]
pub struct CrossChannelReport {
    pub validator: Address,
    pub patterns: Vec<SuspiciousPattern>,
    /// Overall suspicion score (0-100). Higher = more suspicious.
    pub suspicion_score: u32,
    /// Whether the validator should be flagged for review.
    pub flagged: bool,
}

/// Threshold for flagging a validator.
const FLAG_THRESHOLD: u32 = 60;
/// Minimum standard deviation to trigger high-variance flag.
const HIGH_VARIANCE_THRESHOLD: f64 = 40.0;

/// Analyzes cross-channel proof patterns to detect gaming.
pub struct CrossChannelAnalyzer {
    /// History of epoch summaries per validator.
    history: HashMap<Address, Vec<EpochProofSummary>>,
}

impl CrossChannelAnalyzer {
    /// Create a new analyzer.
    pub fn new() -> Self {
        Self {
            history: HashMap::new(),
        }
    }

    /// Record an epoch summary for analysis.
    pub fn record_summary(&mut self, summary: EpochProofSummary) {
        self.history
            .entry(summary.validator)
            .or_default()
            .push(summary);
    }

    /// Analyze a validator's proof patterns and return a report.
    pub fn analyze(&self, validator: &Address) -> CrossChannelReport {
        let empty = vec![];
        let summaries = self.history.get(validator).unwrap_or(&empty);

        let mut patterns = Vec::new();
        let mut suspicion_score: u32 = 0;

        if let Some(latest) = summaries.last() {
            let scores = [
                ("Processing", latest.processing_score),
                ("Gpu", latest.gpu_score),
                ("Storage", latest.storage_score),
                ("Ram", latest.ram_score),
                ("Bandwidth", latest.bandwidth_score),
            ];

            // Check for single-channel dominance.
            let max_score = scores.iter().map(|s| s.1).max().unwrap_or(0);
            let nonzero_count = scores.iter().filter(|s| s.1 > 0).count();

            if max_score > 80 && nonzero_count == 1 {
                let dominant = scores.iter().find(|s| s.1 == max_score).unwrap();
                patterns.push(SuspiciousPattern::SingleChannelDominance {
                    channel: dominant.0,
                    score: dominant.1,
                });
                suspicion_score += 40;
            }

            // Check for high variance.
            let score_values: Vec<f64> = scores.iter().map(|s| s.1 as f64).collect();
            let mean = score_values.iter().sum::<f64>() / score_values.len() as f64;
            let variance = score_values.iter()
                .map(|v| (v - mean).powi(2))
                .sum::<f64>() / score_values.len() as f64;
            let std_dev = variance.sqrt();

            if std_dev > HIGH_VARIANCE_THRESHOLD {
                patterns.push(SuspiciousPattern::HighVariance {
                    variance: variance as u64,
                });
                suspicion_score += 20;
            }

            // Check for impossible profiles.
            if latest.gpu_score > 80 && latest.processing_score == 0 {
                patterns.push(SuspiciousPattern::ImpossibleProfile {
                    reason: "High GPU score with zero CPU score — real GPUs always have a CPU".into(),
                });
                suspicion_score += 30;
            }

            // Check for score volatility between epochs.
            if summaries.len() >= 2 {
                let prev = &summaries[summaries.len() - 2];
                let deltas = [
                    ("Processing", latest.processing_score as i64 - prev.processing_score as i64),
                    ("Gpu", latest.gpu_score as i64 - prev.gpu_score as i64),
                    ("Storage", latest.storage_score as i64 - prev.storage_score as i64),
                    ("Ram", latest.ram_score as i64 - prev.ram_score as i64),
                    ("Bandwidth", latest.bandwidth_score as i64 - prev.bandwidth_score as i64),
                ];

                for (channel, delta) in deltas {
                    if delta.unsigned_abs() > 70 {
                        patterns.push(SuspiciousPattern::ScoreVolatility { channel, delta });
                        suspicion_score += 15;
                    }
                }
            }
        }

        suspicion_score = suspicion_score.min(100);
        let flagged = suspicion_score >= FLAG_THRESHOLD;

        CrossChannelReport {
            validator: *validator,
            patterns,
            suspicion_score,
            flagged,
        }
    }

    /// Analyze all known validators.
    pub fn analyze_all(&self) -> Vec<CrossChannelReport> {
        self.history.keys().map(|v| self.analyze(v)).collect()
    }

    /// Get the number of tracked validators.
    pub fn tracked_count(&self) -> usize {
        self.history.len()
    }

    /// Clear history for a validator.
    pub fn clear_history(&mut self, validator: &Address) {
        self.history.remove(validator);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        let mut a = [0u8; 32];
        a[0] = n;
        Address(a)
    }

    fn make_summary(addr: Address, cpu: u32, gpu: u32, sto: u32, ram: u32, bw: u32) -> EpochProofSummary {
        EpochProofSummary {
            validator: addr,
            epoch: 0,
            processing_score: cpu,
            gpu_score: gpu,
            storage_score: sto,
            ram_score: ram,
            bandwidth_score: bw,
            diversity_bonus: 0,
        }
    }

    #[test]
    fn item_149_detect_single_channel_dominance() {
        let mut analyzer = CrossChannelAnalyzer::new();
        analyzer.record_summary(make_summary(test_addr(1), 0, 100, 0, 0, 0));

        let report = analyzer.analyze(&test_addr(1));
        assert!(report.flagged);
        assert!(report.patterns.iter().any(|p| matches!(p, SuspiciousPattern::SingleChannelDominance { .. })));
    }

    #[test]
    fn item_149_balanced_scores_not_flagged() {
        let mut analyzer = CrossChannelAnalyzer::new();
        analyzer.record_summary(make_summary(test_addr(1), 80, 70, 75, 65, 80));

        let report = analyzer.analyze(&test_addr(1));
        assert!(!report.flagged);
        assert!(report.patterns.is_empty() || report.suspicion_score < FLAG_THRESHOLD);
    }

    #[test]
    fn item_149_impossible_profile() {
        let mut analyzer = CrossChannelAnalyzer::new();
        analyzer.record_summary(make_summary(test_addr(1), 0, 90, 50, 50, 50));

        let report = analyzer.analyze(&test_addr(1));
        assert!(report.patterns.iter().any(|p| matches!(p, SuspiciousPattern::ImpossibleProfile { .. })));
    }

    #[test]
    fn item_149_score_volatility() {
        let mut analyzer = CrossChannelAnalyzer::new();
        analyzer.record_summary(make_summary(test_addr(1), 90, 90, 90, 90, 90));
        // Sudden drop in all channels.
        let mut s2 = make_summary(test_addr(1), 10, 10, 10, 10, 10);
        s2.epoch = 1;
        analyzer.record_summary(s2);

        let report = analyzer.analyze(&test_addr(1));
        assert!(report.patterns.iter().any(|p| matches!(p, SuspiciousPattern::ScoreVolatility { .. })));
    }

    #[test]
    fn item_149_analyze_all() {
        let mut analyzer = CrossChannelAnalyzer::new();
        analyzer.record_summary(make_summary(test_addr(1), 80, 80, 80, 80, 80));
        analyzer.record_summary(make_summary(test_addr(2), 0, 100, 0, 0, 0));

        let reports = analyzer.analyze_all();
        assert_eq!(reports.len(), 2);
    }

    #[test]
    fn item_149_high_variance() {
        let mut analyzer = CrossChannelAnalyzer::new();
        analyzer.record_summary(make_summary(test_addr(1), 100, 0, 100, 0, 100));

        let report = analyzer.analyze(&test_addr(1));
        assert!(report.patterns.iter().any(|p| matches!(p, SuspiciousPattern::HighVariance { .. })));
    }
}
