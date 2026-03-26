//! Item 155: Proof channel weights from genesis.
//!
//! Read channel weights from genesis config instead of hardcoding.
//! Provides a `ChannelWeights` struct that can be initialized from
//! genesis JSON or use sensible defaults.

use commputer_core::proof::ResourceChannel;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Channel weights configuration, typically loaded from genesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelWeights {
    /// Weight for Processing (CPU) channel.
    pub processing: f64,
    /// Weight for GPU channel.
    pub gpu: f64,
    /// Weight for Storage channel.
    pub storage: f64,
    /// Weight for RAM channel.
    pub ram: f64,
    /// Weight for Bandwidth channel.
    pub bandwidth: f64,
}

impl Default for ChannelWeights {
    fn default() -> Self {
        Self {
            processing: 0.25,
            gpu: 0.25,
            storage: 0.20,
            ram: 0.15,
            bandwidth: 0.15,
        }
    }
}

impl ChannelWeights {
    /// Create weights from a HashMap (e.g., parsed from genesis JSON).
    pub fn from_map(map: &HashMap<String, f64>) -> Self {
        Self {
            processing: map.get("processing").or(map.get("Processing")).copied().unwrap_or(0.25),
            gpu: map.get("gpu").or(map.get("Gpu")).copied().unwrap_or(0.25),
            storage: map.get("storage").or(map.get("Storage")).copied().unwrap_or(0.20),
            ram: map.get("ram").or(map.get("Ram")).copied().unwrap_or(0.15),
            bandwidth: map.get("bandwidth").or(map.get("Bandwidth")).copied().unwrap_or(0.15),
        }
    }

    /// Try to parse weights from a genesis JSON value.
    /// Looks for a "channel_weights" key in the JSON object.
    pub fn from_genesis_json(genesis: &serde_json::Value) -> Self {
        if let Some(weights) = genesis.get("channel_weights") {
            if let Ok(cw) = serde_json::from_value::<ChannelWeights>(weights.clone()) {
                return cw;
            }
        }
        Self::default()
    }

    /// Get the weight for a specific channel.
    pub fn weight_for(&self, channel: ResourceChannel) -> f64 {
        match channel {
            ResourceChannel::Processing => self.processing,
            ResourceChannel::Gpu => self.gpu,
            ResourceChannel::Storage => self.storage,
            ResourceChannel::Ram => self.ram,
            ResourceChannel::Bandwidth => self.bandwidth,
        }
    }

    /// Get all weights as a HashMap.
    pub fn as_map(&self) -> HashMap<ResourceChannel, f64> {
        let mut map = HashMap::new();
        map.insert(ResourceChannel::Processing, self.processing);
        map.insert(ResourceChannel::Gpu, self.gpu);
        map.insert(ResourceChannel::Storage, self.storage);
        map.insert(ResourceChannel::Ram, self.ram);
        map.insert(ResourceChannel::Bandwidth, self.bandwidth);
        map
    }

    /// Normalize weights so they sum to 1.0.
    pub fn normalize(&mut self) {
        let total = self.processing + self.gpu + self.storage + self.ram + self.bandwidth;
        if total > 0.0 {
            self.processing /= total;
            self.gpu /= total;
            self.storage /= total;
            self.ram /= total;
            self.bandwidth /= total;
        }
    }

    /// Validate that all weights are non-negative and sum to approximately 1.0.
    pub fn validate(&self) -> Result<(), String> {
        if self.processing < 0.0 || self.gpu < 0.0 || self.storage < 0.0
            || self.ram < 0.0 || self.bandwidth < 0.0
        {
            return Err("Channel weights must be non-negative".into());
        }

        let total = self.processing + self.gpu + self.storage + self.ram + self.bandwidth;
        if (total - 1.0).abs() > 0.01 {
            return Err(format!("Channel weights must sum to ~1.0, got {:.4}", total));
        }

        Ok(())
    }

    /// Apply weights to raw channel scores, returning weighted scores.
    pub fn apply_weights(
        &self,
        processing_score: u32,
        gpu_score: u32,
        storage_score: u32,
        ram_score: u32,
        bandwidth_score: u32,
    ) -> f64 {
        processing_score as f64 * self.processing
            + gpu_score as f64 * self.gpu
            + storage_score as f64 * self.storage
            + ram_score as f64 * self.ram
            + bandwidth_score as f64 * self.bandwidth
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_155_default_weights_valid() {
        let weights = ChannelWeights::default();
        assert!(weights.validate().is_ok());
    }

    #[test]
    fn item_155_from_genesis_json() {
        let genesis = serde_json::json!({
            "channel_weights": {
                "processing": 0.30,
                "gpu": 0.30,
                "storage": 0.15,
                "ram": 0.15,
                "bandwidth": 0.10
            }
        });

        let weights = ChannelWeights::from_genesis_json(&genesis);
        assert_eq!(weights.processing, 0.30);
        assert_eq!(weights.gpu, 0.30);
        assert!(weights.validate().is_ok());
    }

    #[test]
    fn item_155_from_genesis_json_missing_uses_defaults() {
        let genesis = serde_json::json!({});
        let weights = ChannelWeights::from_genesis_json(&genesis);
        assert_eq!(weights.processing, 0.25);
    }

    #[test]
    fn item_155_weight_for_channel() {
        let weights = ChannelWeights::default();
        assert_eq!(weights.weight_for(ResourceChannel::Processing), 0.25);
        assert_eq!(weights.weight_for(ResourceChannel::Bandwidth), 0.15);
    }

    #[test]
    fn item_155_normalize() {
        let mut weights = ChannelWeights {
            processing: 1.0,
            gpu: 1.0,
            storage: 1.0,
            ram: 1.0,
            bandwidth: 1.0,
        };
        weights.normalize();
        assert!((weights.processing - 0.2).abs() < 0.001);
        assert!(weights.validate().is_ok());
    }

    #[test]
    fn item_155_apply_weights() {
        let weights = ChannelWeights {
            processing: 0.2,
            gpu: 0.2,
            storage: 0.2,
            ram: 0.2,
            bandwidth: 0.2,
        };
        let score = weights.apply_weights(100, 100, 100, 100, 100);
        assert!((score - 100.0).abs() < 0.001);

        let score2 = weights.apply_weights(100, 0, 0, 0, 0);
        assert!((score2 - 20.0).abs() < 0.001);
    }

    #[test]
    fn item_155_from_map() {
        let mut map = HashMap::new();
        map.insert("processing".to_string(), 0.4);
        map.insert("gpu".to_string(), 0.3);
        map.insert("storage".to_string(), 0.1);
        map.insert("ram".to_string(), 0.1);
        map.insert("bandwidth".to_string(), 0.1);

        let weights = ChannelWeights::from_map(&map);
        assert_eq!(weights.processing, 0.4);
        assert!(weights.validate().is_ok());
    }

    #[test]
    fn item_155_invalid_weights() {
        let weights = ChannelWeights {
            processing: -0.1,
            gpu: 0.3,
            storage: 0.3,
            ram: 0.3,
            bandwidth: 0.2,
        };
        assert!(weights.validate().is_err());
    }

    #[test]
    fn item_155_as_map() {
        let weights = ChannelWeights::default();
        let map = weights.as_map();
        assert_eq!(map.len(), 5);
        assert_eq!(map[&ResourceChannel::Processing], 0.25);
    }
}
