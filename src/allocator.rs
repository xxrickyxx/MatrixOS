use linked_list_allocator::LockedHeap;

#[global_allocator]
pub static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub static mut HEAP_MEM: [u8; 16 * 1024 * 1024] = [0u8; 16 * 1024 * 1024];

pub fn init() {
    unsafe {
        // Inizializziamo l'allocatore usando il buffer statico nel kernel.
        // Questo è sicuro perché Limine mappa tutto il kernel come scrivibile.
        ALLOCATOR.lock().init(HEAP_MEM.as_mut_ptr(), HEAP_MEM.len());
    }
}
