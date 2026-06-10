//! pouw-sim — Monte-Carlo tournament entry point.
//! Placeholder until Task 14 wires the tournament; the `[[bin]]` target declared
//! in Cargo.toml needs a source file so the crate builds from the skeleton on.

/// Adversarial agent strategies (Task 13). Declared here so the module compiles into the
/// `pouw-sim` binary and its unit tests run under `cargo test -p commputer-pouw`. Task 14
/// adds `mod tournament;` and replaces `main` with the tournament driver.
///
/// `allow(dead_code)`: the strategies are consumed by the tournament driver that lands in
/// Task 14; until then they are exercised only by their own unit tests, so the public API
/// reads as unused. The attribute is scoped to this module and removed once Task 14 wires
/// the consumers.
#[allow(dead_code)]
mod agents;

fn main() {
    println!("pouw-sim: not yet implemented (see Task 14)");
}
