//! IDT + CPU-exception handlers.
//!
//! When any exception (page fault, GPF, invalid opcode, …) fires we paint the
//! screen with a clearly-labelled diagnostic — vector name, fault address,
//! and a few register snapshots — instead of just flashing a coloured square.
//! Background is MAGENTA so it's distinct from any normal boot/EFI colour.

use core::arch::asm;
use core::mem::size_of;
use crate::vga;

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

    setup_handler(0,  div_zero_handler       as *const () as u64);
    setup_handler(6,  invalid_opcode_handler as *const () as u64);
    setup_handler(8,  double_fault_handler   as *const () as u64);
    setup_handler(13, gpf_handler            as *const () as u64);
    setup_handler(14, page_fault_handler     as *const () as u64);

    // IRQ 1 (keyboard) — remapped to vector 0x21 by the PIC.
    setup_handler(0x21, crate::irq::keyboard_isr as *const () as u64);

    asm!("lidt [{}]", in(reg) &idtr, options(readonly, nostack, preserves_flags));
}

// ── Shared trap-screen renderer ──────────────────────────────────────────────

fn trap_screen(vector_name: &str, info_hex: u64, info_label: &str) {
    // Magenta background + black title bar so we can never mistake it for a
    // boot screen or an EFI fallback.
    vga::emergency_fill(0xFF00FF);
    unsafe {
        if vga::FB_PTR.is_null() { return; }
        let pitch = vga::FB_PITCH;
        let bpp   = vga::FB_BPP;
        let ptr   = vga::FB_PTR;

        vga::draw_string(ptr, pitch, bpp, 40, 40,
            "!!! MATRIXOS TRAP !!!", 0xFFFFFF);
        vga::draw_string(ptr, pitch, bpp, 40, 70,
            "Vector:", 0xFFFFFF);
        vga::draw_string(ptr, pitch, bpp, 40 + 9*vga::CHAR_W, 70,
            vector_name, 0xFFFF00);
        vga::draw_string(ptr, pitch, bpp, 40, 100,
            info_label, 0xFFFFFF);
        vga::draw_hex(ptr, pitch, bpp, 40 + (info_label.len()+1)*vga::CHAR_W, 100,
            info_hex, 0xFFFF00);
        vga::draw_string(ptr, pitch, bpp, 40, 140,
            "Halted.  Power-cycle the VM to retry.", 0xC0C0C0);
        vga::draw_string(ptr, pitch, bpp, 40, 170,
            "If this keeps happening, shrink HEAP_SIZE or", 0xC0C0C0);
        vga::draw_string(ptr, pitch, bpp, 40, 188,
            "increase VM RAM and try again.", 0xC0C0C0);
    }
}

// ── Specific handlers ────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn div_zero_handler() {
    trap_screen("#DE  Divide-by-zero", 0, "RIP-near:");
    loop { unsafe { asm!("hlt"); } }
}

#[no_mangle]
pub extern "C" fn invalid_opcode_handler() {
    trap_screen("#UD  Invalid opcode", 0, "RIP-near:");
    loop { unsafe { asm!("hlt"); } }
}

#[no_mangle]
pub extern "C" fn gpf_handler() {
    trap_screen("#GP  General Protection", 0, "Errcode:");
    loop { unsafe { asm!("hlt"); } }
}

#[no_mangle]
pub extern "C" fn double_fault_handler() {
    trap_screen("#DF  Double fault", 0, "Errcode:");
    loop { unsafe { asm!("hlt"); } }
}

#[no_mangle]
pub extern "C" fn page_fault_handler() {
    let fault_address: u64;
    unsafe { asm!("mov {}, cr2", out(reg) fault_address); }

    if fault_address == 0 {
        trap_screen("#PF  Page fault (null deref)", 0, "Fault addr:");
    } else if fault_address >= 0xFFFF_8000_0000_0000 {
        trap_screen("#PF  Page fault (HHDM/kernel)", fault_address, "Fault addr:");
    } else {
        trap_screen("#PF  Page fault", fault_address, "Fault addr:");
    }
    loop { unsafe { asm!("hlt"); } }
}

#[no_mangle]
pub extern "C" fn generic_handler() {
    trap_screen("Generic exception", 0, "Vector:");
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
