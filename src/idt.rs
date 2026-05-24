use core::arch::asm;
use core::mem::size_of;

#[derive(Copy, Clone)]
#[repr(C, packed)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    flags: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

#[repr(C, packed)]
struct Idtr {
    limit: u16,
    base: u64,
}

static mut IDT: [IdtEntry; 256] = [IdtEntry {
    offset_low: 0,
    selector: 0,
    ist: 0,
    flags: 0,
    offset_mid: 0,
    offset_high: 0,
    reserved: 0,
}; 256];

pub unsafe fn init() {
    let idtr = Idtr {
        limit: (size_of::<IdtEntry>() * 256 - 1) as u16,
        base: core::ptr::addr_of!(IDT) as u64,
    };

    for i in 0..256 {
        setup_handler(i, generic_handler as *const () as u64);
    }

    // Diagnostica specifica per capire il freeze allo Stage 2
    setup_handler(0, div_zero_handler as *const () as u64);
    setup_handler(6, invalid_opcode_handler as *const () as u64);
    setup_handler(13, gpf_handler as *const () as u64);
    setup_handler(14, page_fault_handler as *const () as u64);

    asm!("lidt [{}]", in(reg) &idtr, options(readonly, nostack, preserves_flags));
}

use crate::vga;

#[no_mangle] pub extern "C" fn div_zero_handler() { vga::emergency_fill(0x0000FF); loop { unsafe { asm!("hlt"); } } }
#[no_mangle] pub extern "C" fn invalid_opcode_handler() { vga::emergency_fill(0x00FF00); loop { unsafe { asm!("hlt"); } } }
#[no_mangle] pub extern "C" fn gpf_handler() { vga::emergency_fill(0xFF0000); loop { unsafe { asm!("hlt"); } } }

#[no_mangle] 
pub extern "C" fn page_fault_handler() { 
    let fault_address: u64;
    unsafe {
        asm!("mov {}, cr2", out(reg) fault_address);
    }
    
    if fault_address == 0 {
        vga::emergency_fill(0xFFFF00); // GIALLO: Null Pointer
    } else {
        vga::emergency_fill(0xFF8800); // ARANCIONE: Indirizzo specifico
        unsafe {
            vga::draw_hex(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP, 50, 50, fault_address, 0xFFFFFF);
        }
    }
    
    loop { unsafe { asm!("hlt"); } } 
}

unsafe fn setup_handler(vector: usize, handler: u64) {
    IDT[vector] = IdtEntry {
        offset_low: handler as u16,
        selector: 0x28,
        ist: 0,
        flags: 0x8E,
        offset_mid: (handler >> 16) as u16,
        offset_high: (handler >> 32) as u32,
        reserved: 0,
    };
}

#[no_mangle]
pub extern "C" fn generic_handler() {
    loop { unsafe { asm!("hlt"); } }
}
