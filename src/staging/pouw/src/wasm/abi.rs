//! Host side of the zero-import guest ABI (spec §7): export binding with exact
//! signatures, packed-i64 splitting, and bounds checks against the fixed memory.
//! New file; wired via wasm/mod.rs. No existing-file changes.

use crate::wasm::error::ExecError;
use wasmi::{Instance, Memory, Store, TypedFunc};

pub struct AbiHandles {
    pub memory: Memory,
    pub alloc: TypedFunc<i32, i32>,
    pub run: TypedFunc<(i32, i32), i64>,
}

/// Bind the three required exports with EXACT signatures. Validation (rule 8)
/// already guaranteed presence+kind; a wrong signature deterministically folds
/// to Rejected here.
pub fn bind<T>(store: &Store<T>, instance: &Instance) -> Result<AbiHandles, ExecError> {
    let memory = instance
        .get_memory(store, "memory")
        .ok_or_else(|| ExecError::Rejected("export `memory` missing or not a memory".into()))?;
    let alloc = instance
        .get_typed_func::<i32, i32>(store, "alloc")
        .map_err(|e| ExecError::Rejected(format!("export `alloc` signature: {e}")))?;
    let run = instance
        .get_typed_func::<(i32, i32), i64>(store, "run")
        .map_err(|e| ExecError::Rejected(format!("export `run` signature: {e}")))?;
    Ok(AbiHandles { memory, alloc, run })
}

/// Split run()'s packed i64 into (out_ptr, out_len). Both halves are decoded
/// as u32 unconditionally — a guest that mis-packs produces garbage that the
/// bounds check below rejects deterministically (spec §7).
pub fn unpack(packed: i64) -> (u32, u32) {
    (((packed as u64) >> 32) as u32, packed as u64 as u32)
}

/// Check [ptr, ptr+len) lies inside the fixed linear memory.
pub fn check_bounds(mem_len: usize, ptr: u32, len: u32, what: &str) -> Result<(), ExecError> {
    let end = ptr as u64 + len as u64; // cannot overflow: u32 + u32 fits in u64
    if end > mem_len as u64 {
        return Err(ExecError::AbiViolation(format!(
            "{what} [{ptr}, {end}) exceeds memory size {mem_len}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the "decoded as u32 unconditionally" claim: a negative packed i64
    /// must decode to large u32 halves (then fail bounds), never sign-extend.
    #[test]
    fn unpack_negative_packed_value_decodes_as_u32() {
        assert_eq!(unpack(-1), (u32::MAX, u32::MAX));
        assert_eq!(unpack(0), (0, 0));
        assert_eq!(unpack(((7u64 << 32) | 9) as i64), (7, 9));
    }

    /// Pins exact-boundary acceptance: [ptr, ptr+len) ending exactly at
    /// mem_len passes; one past fails. (The off-by-one a refactor could break.)
    #[test]
    fn check_bounds_accepts_exact_end_rejects_past_end() {
        assert!(check_bounds(100, 90, 10, "x").is_ok());
        assert!(check_bounds(100, 91, 10, "x").is_err());
        assert!(check_bounds(100, 100, 0, "x").is_ok());
    }
}
