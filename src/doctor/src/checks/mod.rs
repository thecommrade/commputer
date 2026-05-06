// checks/mod.rs — module wiring for commputer-doctor checks
//
// WHAT IT DOES:
//   Re-exports the four standalone check modules used by main.rs.
//
// WHERE IT SHOULD GO:
//   src/doctor/src/checks/mod.rs alongside the other files in this directory.

pub mod cloud_ip;
pub mod genesis;
pub mod ntp;
pub mod port_reachability;
