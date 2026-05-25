use core::mem::MaybeUninit;
use linked_list_allocator::LockedHeap;

// 32 MB heap.  Limine appears to fail to fully zero-fill the BSS
// segment when it exceeds ~40 MB on this VirtualBox+EFI setup
// (symptom: orange post-boot EFI screen, kernel never starts).  32 MB
// is enough for two KV caches (~9 + 7 MB) plus the shared scratch
// (~12 MB worst case) plus some headroom.
const HEAP_SIZE: usize = 32 * 1024 * 1024;

#[global_allocator]
pub static ALLOCATOR: LockedHeap = LockedHeap::empty();

// MaybeUninit goes to BSS — zero-overhead in the ELF file.
static mut HEAP_STORAGE: MaybeUninit<[u8; HEAP_SIZE]> = MaybeUninit::uninit();

pub fn init() {
    unsafe {
        ALLOCATOR.lock().init(HEAP_STORAGE.as_mut_ptr() as *mut u8, HEAP_SIZE);
    }
}
