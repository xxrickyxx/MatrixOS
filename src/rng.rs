//! Random number generator.
//!
//! Tries hardware RDRAND (CPUID leaf 1, ECX bit 30) first.  Falls back to
//! a xorshift64 PRNG seeded by TSC at boot.  Fine for transaction IDs,
//! cookies, app names — NOT for cryptography.

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::math;

static mut HAS_RDRAND: bool = false;
static PRNG_STATE: AtomicU64 = AtomicU64::new(0xC0FFEE_5EED_BABE);

/// Probe CPUID for RDRAND at boot.  Safe to call repeatedly.
pub fn init() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let ecx: u32;
        asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "pop rbx",
            out("eax") _,
            out("ecx") ecx,
            out("edx") _,
        );
        HAS_RDRAND = (ecx & (1 << 30)) != 0;
        // Seed the fallback PRNG with TSC and a constant — xorshift64 needs
        // a non-zero state.
        let seed = math::rdtsc() ^ 0xA5A5_5A5A_DEAD_BEEF;
        PRNG_STATE.store(if seed == 0 { 1 } else { seed }, Ordering::Relaxed);
    }
}

/// Try a single RDRAND instruction; returns None on retry-exhaustion.
unsafe fn rdrand64() -> Option<u64> {
    let mut v: u64;
    let mut ok: u8;
    for _ in 0..16 {
        asm!(
            "rdrand {v}",
            "setc {ok}",
            v = out(reg) v,
            ok = out(reg_byte) ok,
            options(nostack, preserves_flags, nomem)
        );
        if ok != 0 { return Some(v); }
    }
    None
}

/// xorshift64: deterministic but fast.  Reads + writes the global state.
fn xorshift_next() -> u64 {
    let mut s = PRNG_STATE.load(Ordering::Relaxed);
    s ^= s << 13;
    s ^= s >> 7;
    s ^= s << 17;
    PRNG_STATE.store(s, Ordering::Relaxed);
    s
}

/// 64 bits of randomness — hardware-backed when possible.
pub fn rand_u64() -> u64 {
    unsafe {
        if HAS_RDRAND {
            if let Some(v) = rdrand64() { return v; }
        }
    }
    xorshift_next()
}

pub fn rand_u32() -> u32 { rand_u64() as u32 }

/// Returns true if the hardware RNG was detected.
pub fn has_hardware() -> bool { unsafe { HAS_RDRAND } }
