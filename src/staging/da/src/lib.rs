//! commputer-da — data-availability sampling for the PoUW verification game.
//! Spec: src/staging/docs/2026-06-12-data-availability-design.md
//! No trusted setup: Merkle-over-Reed-Solomon + sampling + sha256 re-bind.
pub mod params;
pub mod chunk;
pub mod code;
pub mod merkle;
pub mod commit;
pub mod transport;
pub mod providers;
pub mod sampling;
pub mod facade;
pub mod adapter;
