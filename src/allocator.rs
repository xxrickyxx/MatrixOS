//! Heap allocator.
//!
//! Instead of carving the heap out of a giant static BSS array (which forces
//! Limine to zero-fill many MB on every boot — the cause of the "orange
//! screen" hang past ~40 MB), we now ask Limine for the biggest usable RAM
//! region in its memory map and use *that* as our heap.
//!
//! Falls back to a small static BSS heap if the memmap is unavailable, so
//! the kernel still boots in pathological cases.

use linked_list_allocator::LockedHeap;
use limine::request::MemmapRequest;
use limine::memmap::MEMMAP_USABLE;
use core::mem::MaybeUninit;

#[used] #[link_section = ".requests"]
static MEMMAP_REQUEST: MemmapRequest = MemmapRequest::new();

/// Fallback heap (small enough that Limine can definitely zero it).
const FALLBACK_HEAP: usize = 16 * 1024 * 1024;
static mut FALLBACK_STORAGE: MaybeUninit<[u8; FALLBACK_HEAP]> = MaybeUninit::uninit();

#[global_allocator]
pub static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Returns (base, size) of the heap actually installed.  Useful for the
/// sidebar so the user sees real heap size.
pub static mut HEAP_BASE: u64    = 0;
pub static mut HEAP_BYTES: usize = 0;

/// Initialise the heap.  Pass the HHDM offset from Limine so we can translate
/// physical USABLE regions into virtual addresses the kernel can dereference.
pub fn init_with_memmap(hhdm: u64) {
    if let Some(resp) = MEMMAP_REQUEST.response() {
        // Pick the largest USABLE entry.
        let mut best_base: u64 = 0;
        let mut best_len:  u64 = 0;
        for e in resp.entries() {
            if e.type_ == MEMMAP_USABLE && e.length > best_len {
                best_base = e.base;
                best_len  = e.length;
            }
        }
        // Cap heap at 1 GiB — way more than we need, even with big KV caches.
        let len = best_len.min(1024 * 1024 * 1024) as usize;
        if best_base != 0 && len >= 16 * 1024 * 1024 {
            // Translate physical → HHDM virtual.
            let virt = best_base.wrapping_add(hhdm);
            unsafe {
                ALLOCATOR.lock().init(virt as *mut u8, len);
                HEAP_BASE  = virt;
                HEAP_BYTES = len;
            }
            return;
        }
    }
    // Fallback to the small static heap.
    unsafe {
        ALLOCATOR.lock().init(FALLBACK_STORAGE.as_mut_ptr() as *mut u8, FALLBACK_HEAP);
        HEAP_BASE  = FALLBACK_STORAGE.as_mut_ptr() as u64;
        HEAP_BYTES = FALLBACK_HEAP;
    }
}

/// Old-style entry retained for callers; uses the fallback static heap only.
pub fn init() {
    unsafe {
        ALLOCATOR.lock().init(FALLBACK_STORAGE.as_mut_ptr() as *mut u8, FALLBACK_HEAP);
        HEAP_BASE  = FALLBACK_STORAGE.as_mut_ptr() as u64;
        HEAP_BYTES = FALLBACK_HEAP;
    }
}
