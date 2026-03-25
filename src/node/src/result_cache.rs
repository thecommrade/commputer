use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// A cached compute result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedResult {
    pub result_hash: [u8; 32],
    pub output: Vec<u8>,
    pub cached_at_height: u64,
    pub hit_count: u64,
}

/// In-memory cache for compute job results.
pub struct ResultCache {
    pub cache: HashMap<[u8; 32], CachedResult>,
    pub max_entries: usize,
}

impl ResultCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            cache: HashMap::new(),
            max_entries,
        }
    }

    /// Check if a result is cached for a given job spec hash.
    pub fn check_cache(&self, job_spec_hash: &[u8; 32]) -> Option<&CachedResult> {
        self.cache.get(job_spec_hash)
    }

    /// Cache a result. Evicts oldest entry if cache is full.
    pub fn cache_result(
        &mut self,
        job_spec_hash: [u8; 32],
        result_hash: [u8; 32],
        output: Vec<u8>,
        current_height: u64,
    ) {
        if self.cache.len() >= self.max_entries && !self.cache.contains_key(&job_spec_hash) {
            self.evict_oldest();
        }
        let entry = CachedResult {
            result_hash,
            output,
            cached_at_height: current_height,
            hit_count: 0,
        };
        self.cache.insert(job_spec_hash, entry);
    }

    /// Evict the least recently used (lowest hit_count) entry.
    pub fn evict_oldest(&mut self) {
        if self.cache.is_empty() {
            return;
        }
        // Find the entry with the lowest hit count (LRU approximation)
        let key_to_remove = self
            .cache
            .iter()
            .min_by_key(|(_, v)| v.hit_count)
            .map(|(k, _)| *k);

        if let Some(key) = key_to_remove {
            self.cache.remove(&key);
        }
    }

    /// Record a cache hit, incrementing the hit count.
    pub fn record_hit(&mut self, job_spec_hash: &[u8; 32]) {
        if let Some(entry) = self.cache.get_mut(job_spec_hash) {
            entry.hit_count += 1;
        }
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hash(val: u8) -> [u8; 32] {
        let mut h = [0u8; 32];
        h[0] = val;
        h
    }

    #[test]
    fn test_cache_and_retrieve() {
        let mut cache = ResultCache::new(100);
        let spec = test_hash(1);
        let result = test_hash(2);
        cache.cache_result(spec, result, vec![1, 2, 3], 100);

        let cached = cache.check_cache(&spec).unwrap();
        assert_eq!(cached.result_hash, result);
        assert_eq!(cached.output, vec![1, 2, 3]);
        assert_eq!(cached.cached_at_height, 100);
    }

    #[test]
    fn test_cache_miss() {
        let cache = ResultCache::new(100);
        assert!(cache.check_cache(&test_hash(99)).is_none());
    }

    #[test]
    fn test_eviction() {
        let mut cache = ResultCache::new(2);
        cache.cache_result(test_hash(1), test_hash(10), vec![], 1);
        cache.cache_result(test_hash(2), test_hash(20), vec![], 2);

        // Hit entry 2 so entry 1 has lowest hit count
        cache.record_hit(&test_hash(2));

        // Adding a third should evict entry 1 (lowest hit count)
        cache.cache_result(test_hash(3), test_hash(30), vec![], 3);

        assert_eq!(cache.len(), 2);
        assert!(cache.check_cache(&test_hash(1)).is_none()); // evicted
        assert!(cache.check_cache(&test_hash(2)).is_some());
        assert!(cache.check_cache(&test_hash(3)).is_some());
    }

    #[test]
    fn test_hit_count() {
        let mut cache = ResultCache::new(10);
        let spec = test_hash(5);
        cache.cache_result(spec, test_hash(50), vec![], 10);
        cache.record_hit(&spec);
        cache.record_hit(&spec);
        cache.record_hit(&spec);

        let cached = cache.check_cache(&spec).unwrap();
        assert_eq!(cached.hit_count, 3);
    }
}
