//! Realism-showcase guest (spec §9.D): a real Rust program compiled to wasm32
//! that passes the determinism gate. Integer-only; static bump arena (NO
//! dlmalloc => no memory.grow — spec §4 guest constraint b).
//! Transform: 32-byte output = 4 u64 LE lanes of an xorshift64* stream seeded
//! by FNV-1a over the input, 1000 rounds per lane. Mirrored natively by
//! `native_reference()` in tests/wasm_runtime.rs — keep both in sync.
#![no_std]

const ARENA_SIZE: usize = 64 * 1024;
static mut ARENA: [u8; ARENA_SIZE] = [0; ARENA_SIZE];
static mut NEXT: usize = 0;

/// Bump allocator over the static arena. Never grows memory; exhaustion traps
/// (deterministically) via unreachable.
#[no_mangle]
pub extern "C" fn alloc(len: i32) -> i32 {
    unsafe {
        let base = core::ptr::addr_of_mut!(ARENA) as *mut u8;
        let start = NEXT;
        let len = len as usize;
        if len > ARENA_SIZE || start > ARENA_SIZE - len {
            core::arch::wasm32::unreachable()
        }
        NEXT = start + len;
        base.add(start) as i32
    }
}

#[no_mangle]
pub extern "C" fn run(ptr: i32, len: i32) -> i64 {
    let input = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };

    // FNV-1a 64 seed over the input.
    let mut seed: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in input {
        seed ^= b as u64;
        seed = seed.wrapping_mul(0x0000_0100_0000_01B3);
    }
    let mut state = if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed };

    // 4 lanes x 1000 xorshift64 rounds — integer-only, loops enough to meter.
    let mut out = [0u8; 32];
    for lane in 0..4 {
        for _ in 0..1000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
        }
        out[lane * 8..lane * 8 + 8].copy_from_slice(&state.to_le_bytes());
    }

    let out_ptr = alloc(32);
    unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), out_ptr as *mut u8, 32) };
    // Canonical packing (spec §7): unsigned shift, never sign-extending.
    (((out_ptr as u32 as u64) << 32) | 32u64) as i64
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}
