use core::mem::MaybeUninit;
use linked_list_allocator::LockedHeap;

// 64 MB heap — enough for two KV caches (~10 MB each), the shared
// forward scratch (~10 MB), tokenization buffers, and headroom for
// runtime Vec allocations.  Still fits comfortably in the 8 GB VM.
const HEAP_SIZE: usize = 64 * 1024 * 1024;

#[global_allocator]
pub static ALLOCATOR: LockedHeap = LockedHeap::empty();

// MaybeUninit goes to BSS — zero-overhead in the ELF file.
static mut HEAP_STORAGE: MaybeUninit<[u8; HEAP_SIZE]> = MaybeUninit::uninit();

pub fn init() {
    unsafe {
        ALLOCATOR.lock().init(HEAP_STORAGE.as_mut_ptr() as *mut u8, HEAP_SIZE);
    }
}
