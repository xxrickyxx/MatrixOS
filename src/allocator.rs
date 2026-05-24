use core::mem::MaybeUninit;
use linked_list_allocator::LockedHeap;

const HEAP_SIZE: usize = 20 * 1024 * 1024;

#[global_allocator]
pub static ALLOCATOR: LockedHeap = LockedHeap::empty();

// MaybeUninit goes to BSS — zero-overhead in the ELF file.
static mut HEAP_STORAGE: MaybeUninit<[u8; HEAP_SIZE]> = MaybeUninit::uninit();

pub fn init() {
    unsafe {
        ALLOCATOR.lock().init(HEAP_STORAGE.as_mut_ptr() as *mut u8, HEAP_SIZE);
    }
}
