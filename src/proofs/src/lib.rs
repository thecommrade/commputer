pub mod cpu;
pub mod gpu;
pub mod storage_proof;
pub mod ram;
pub mod bandwidth;
pub mod verifier;
pub mod challenge;

// Block H: Advanced Proof Channels (items 141-160)
pub mod wgpu_prover;          // Item 141: GPU proof with WGPU compute shader
pub mod gpu_fallback;          // Item 142: GPU proof fallback measurement
pub mod merkle_storage;        // Item 143: Storage proof with merkle verification
pub mod storage_rotation;      // Item 144: Storage proof rotation
pub mod ram_latency;           // Item 145: RAM proof with DRAM latency measurement
pub mod bidirectional_bandwidth; // Item 146: Bandwidth proof bidirectional
pub mod fair_scheduler;        // Item 147: Proof challenge scheduling fairness
pub mod cross_channel;         // Item 149: Cross-channel correlation
pub mod difficulty_calibrator; // Item 151: Proof difficulty auto-calibration
pub mod dispute;               // Item 153: Proof result dispute mechanism
pub mod hardware_benchmark;    // Item 154: Hardware benchmark on startup
pub mod channel_weights;       // Item 155: Proof channel weights from genesis
pub mod uptime_prover;         // Item 156: New proof channel: Uptime
pub mod proof_encryption;      // Item 158: Proof result encryption

pub use cpu::CpuProver;
pub use gpu::{GpuProver, gpu_available};
pub use storage_proof::StorageProver;
pub use ram::RamProver;
pub use bandwidth::{BandwidthProver, BandwidthChallenge, BandwidthReport};
pub use verifier::ProofVerifier;
pub use challenge::ChallengeGenerator;

// Block H re-exports
pub use wgpu_prover::WgpuProver;
pub use gpu_fallback::GpuFallbackScorer;
pub use merkle_storage::{MerkleStorageProver, MerkleStorageTree, MerkleProof};
pub use storage_rotation::StorageRotation;
pub use ram_latency::DramLatencyProver;
pub use bidirectional_bandwidth::BidirectionalBandwidth;
pub use fair_scheduler::FairScheduler;
pub use cross_channel::CrossChannelAnalyzer;
pub use difficulty_calibrator::DifficultyCalibrator;
pub use dispute::DisputeManager;
pub use hardware_benchmark::HardwareBenchmark;
pub use channel_weights::ChannelWeights;
pub use uptime_prover::UptimeProver;
pub use proof_encryption::ProofEncryptor;
