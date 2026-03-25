use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Total resource capacity for a validator.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceCap {
    pub cpu_cores: u16,
    pub gpu_vram_mb: u64,
    pub ram_mb: u64,
}

/// A resource reservation tied to a specific job and validator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceReservation {
    pub job_id: [u8; 32],
    pub validator_hex: String,
    pub cpu_cores_reserved: u16,
    pub gpu_vram_reserved: u64,
    pub ram_reserved: u64,
    pub reserved_at_height: u64,
}

/// Pool of active resource reservations.
#[derive(Debug, Default)]
pub struct ReservationPool {
    reservations: HashMap<[u8; 32], ResourceReservation>,
}

impl ReservationPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reserve resources for a job. Returns false if a reservation for this
    /// job already exists or if the validator doesn't have enough capacity.
    pub fn reserve(
        &mut self,
        job_id: [u8; 32],
        validator_hex: String,
        cpu_cores: u16,
        gpu_vram_mb: u64,
        ram_mb: u64,
        height: u64,
        total_capacity: &ResourceCap,
    ) -> bool {
        if self.reservations.contains_key(&job_id) {
            return false;
        }

        // Check available capacity
        let available = self.available_for_validator(&validator_hex, total_capacity);
        if cpu_cores > available.cpu_cores
            || gpu_vram_mb > available.gpu_vram_mb
            || ram_mb > available.ram_mb
        {
            return false;
        }

        self.reservations.insert(job_id, ResourceReservation {
            job_id,
            validator_hex,
            cpu_cores_reserved: cpu_cores,
            gpu_vram_reserved: gpu_vram_mb,
            ram_reserved: ram_mb,
            reserved_at_height: height,
        });
        true
    }

    /// Release a reservation by job ID.
    pub fn release(&mut self, job_id: &[u8; 32]) -> bool {
        self.reservations.remove(job_id).is_some()
    }

    /// Calculate remaining available resources for a validator.
    pub fn available_for_validator(
        &self,
        validator_hex: &str,
        total_capacity: &ResourceCap,
    ) -> ResourceCap {
        let mut used_cpu: u16 = 0;
        let mut used_gpu: u64 = 0;
        let mut used_ram: u64 = 0;

        for r in self.reservations.values() {
            if r.validator_hex == validator_hex {
                used_cpu = used_cpu.saturating_add(r.cpu_cores_reserved);
                used_gpu = used_gpu.saturating_add(r.gpu_vram_reserved);
                used_ram = used_ram.saturating_add(r.ram_reserved);
            }
        }

        ResourceCap {
            cpu_cores: total_capacity.cpu_cores.saturating_sub(used_cpu),
            gpu_vram_mb: total_capacity.gpu_vram_mb.saturating_sub(used_gpu),
            ram_mb: total_capacity.ram_mb.saturating_sub(used_ram),
        }
    }

    /// Get a reservation by job ID.
    pub fn get(&self, job_id: &[u8; 32]) -> Option<&ResourceReservation> {
        self.reservations.get(job_id)
    }

    /// Number of active reservations.
    pub fn count(&self) -> usize {
        self.reservations.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap() -> ResourceCap {
        ResourceCap {
            cpu_cores: 8,
            gpu_vram_mb: 4096,
            ram_mb: 16384,
        }
    }

    #[test]
    fn test_reserve_and_get() {
        let mut pool = ReservationPool::new();
        let cap = cap();
        assert!(pool.reserve([1u8; 32], "v1".into(), 2, 1024, 4096, 100, &cap));
        assert!(pool.get(&[1u8; 32]).is_some());
        assert_eq!(pool.count(), 1);
    }

    #[test]
    fn test_duplicate_reservation() {
        let mut pool = ReservationPool::new();
        let cap = cap();
        assert!(pool.reserve([1u8; 32], "v1".into(), 2, 0, 1024, 100, &cap));
        assert!(!pool.reserve([1u8; 32], "v1".into(), 2, 0, 1024, 101, &cap));
    }

    #[test]
    fn test_release() {
        let mut pool = ReservationPool::new();
        let cap = cap();
        pool.reserve([1u8; 32], "v1".into(), 2, 0, 1024, 100, &cap);
        assert!(pool.release(&[1u8; 32]));
        assert!(!pool.release(&[1u8; 32])); // already released
        assert_eq!(pool.count(), 0);
    }

    #[test]
    fn test_available_after_reservation() {
        let mut pool = ReservationPool::new();
        let cap = cap();
        pool.reserve([1u8; 32], "v1".into(), 3, 1024, 8192, 100, &cap);
        let avail = pool.available_for_validator("v1", &cap);
        assert_eq!(avail.cpu_cores, 5);
        assert_eq!(avail.gpu_vram_mb, 3072);
        assert_eq!(avail.ram_mb, 8192);
    }

    #[test]
    fn test_exceeds_capacity() {
        let mut pool = ReservationPool::new();
        let cap = cap();
        // Try to reserve more than available
        assert!(!pool.reserve([1u8; 32], "v1".into(), 10, 0, 0, 100, &cap));
    }

    #[test]
    fn test_multiple_validators_independent() {
        let mut pool = ReservationPool::new();
        let cap = cap();
        pool.reserve([1u8; 32], "v1".into(), 4, 0, 8192, 100, &cap);
        // v2 has full capacity since it's a different validator
        let avail_v2 = pool.available_for_validator("v2", &cap);
        assert_eq!(avail_v2.cpu_cores, 8);
        assert_eq!(avail_v2.ram_mb, 16384);
    }

    #[test]
    fn test_release_frees_capacity() {
        let mut pool = ReservationPool::new();
        let cap = cap();
        pool.reserve([1u8; 32], "v1".into(), 8, 4096, 16384, 100, &cap);
        let avail = pool.available_for_validator("v1", &cap);
        assert_eq!(avail.cpu_cores, 0);
        pool.release(&[1u8; 32]);
        let avail = pool.available_for_validator("v1", &cap);
        assert_eq!(avail.cpu_cores, 8);
    }
}
