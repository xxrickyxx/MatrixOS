#![no_std]
#![no_main]

extern crate alloc;
pub mod allocator;
pub mod gguf;
pub mod vga;
pub mod keyboard;
pub mod idt;
pub mod tokenizer;
pub mod math;
pub mod model;

#[cfg(target_arch = "x86_64")]
pub mod memory;

use core::panic::PanicInfo;
use limine::request::{FramebufferRequest, ModulesRequest, HhdmRequest};
use limine::BaseRevision;

#[used]
#[link_section = ".requests"]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[link_section = ".requests"]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

#[used]
#[link_section = ".requests"]
static MODULE_REQUEST: ModulesRequest = ModulesRequest::new();

#[used]
#[link_section = ".requests"]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

// --- FUNZIONI DI SUPPORTO ---

unsafe fn enable_sse() {
    // Usiamo assembly diretto per evitare problemi con la crate x86_64 in alcune VM.
    // Questo è identico a ciò che fa Linux all'avvio.
    core::arch::asm!(
        // CR0: clear EM (bit 2), set MP (bit 1)
        "mov rax, cr0",
        "and eax, 0xFFFFFFFB",  // clear EM
        "or  eax, 0x2",          // set MP
        "mov cr0, rax",
        // CR4: set OSFXSR (bit 9) e OSXMMEXCPT (bit 10)
        "mov rax, cr4",
        "or  eax, 0x600",
        "mov cr4, rax",
        out("rax") _,
        options(nostack)
    );
}

/// Piccolo ritardo per dare tempo alla VRAM di essere pronta
#[inline(never)]
fn delay() {
    for _ in 0..500_000 {
        unsafe { core::arch::asm!("nop"); }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // ── STEP 0: INIT SCHERMO (deve essere il PRIMO blocco assoluto) ─────────
    let mut fb_ptr: *mut u8 = core::ptr::null_mut();
    let mut pitch: usize = 0;
    let mut bpp: usize = 0;

    if let Some(fb_res) = FRAMEBUFFER_REQUEST.response() {
        if let Some(fb) = fb_res.framebuffers().first() {
            fb_ptr = fb.address() as *mut u8;
            pitch  = fb.pitch as usize;
            bpp    = fb.bpp as usize / 8;
            let fb_size = pitch * fb.height as usize;
            for i in 0..fb_size { unsafe { *fb_ptr.add(i) = 0; } }
            unsafe { vga::FB_PTR = fb_ptr; vga::FB_PITCH = pitch; vga::FB_BPP = bpp; }
        }
    }

    // Se schermo non disponibile, halt silenzioso
    if fb_ptr.is_null() {
        loop { unsafe { core::arch::asm!("hlt"); } }
    }

    delay();
    vga::draw_string(fb_ptr, pitch, bpp, 20, 20, "MATRIXOS V2.2 - BOOT OK", 0x00FF00);

    // ── STEP 1: HARDWARE (dopo lo schermo, così vediamo se crashano) ────────
    unsafe {
        enable_sse();
        core::arch::asm!("cli");
    }
    vga::draw_string(fb_ptr, pitch, bpp, 20, 40, "STAGE 1: SSE OK", 0x00FF00);

    unsafe { idt::init(); }
    vga::draw_string(fb_ptr, pitch, bpp, 20, 60, "STAGE 2: IDT OK", 0x00FF00);

    // ── STEP 2: VERIFICA REVISIONE LIMINE ──────────────────────────────────
    // NON usiamo assert! (causerebbe panic prima che lo schermo sia pronto).
    // Usiamo un controllo manuale e stampiamo un errore visibile.
    if !BASE_REVISION.is_supported() {
        vga::draw_string(fb_ptr, pitch, bpp, 20, 60, "FATAL: LIMINE REVISION NOT SUPPORTED", 0xFF0000);
        loop { unsafe { core::arch::asm!("hlt"); } }
    }
    vga::draw_string(fb_ptr, pitch, bpp, 20, 60, "STAGE 2: LIMINE REVISION OK", 0x00FF00);

    // ── STEP 3: MEMORIA (HEAP) ──────────────────────────────────────────────
    allocator::init();
    vga::draw_string(fb_ptr, pitch, bpp, 20, 80, "STAGE 3: HEAP ONLINE (16MB)", 0x00FF00);

    // ── STEP 4: HHDM (Higher Half Direct Map) ──────────────────────────────
    let hhdm_offset: u64 = if let Some(r) = HHDM_REQUEST.response() {
        vga::draw_string(fb_ptr, pitch, bpp, 20, 100, "STAGE 4: HHDM ONLINE", 0x00FF00);
        r.offset
    } else {
        // HHDM non disponibile: usiamo offset 0 e proviamo ugualmente
        vga::draw_string(fb_ptr, pitch, bpp, 20, 100, "STAGE 4: HHDM N/A (FALLBACK 0)", 0xFFAA00);
        0x80000000u64
    };

    // ── STEP 5: CARICAMENTO MODULO GGUF ────────────────────────────────────
    vga::draw_string(fb_ptr, pitch, bpp, 20, 120, "STAGE 5: LOADING AI MODULE...", 0x00FFFF);

    let mut cursor_y: usize = 140;

    if let Some(mod_res) = MODULE_REQUEST.response() {
        let modules = mod_res.modules();
        if modules.is_empty() {
            vga::draw_string(fb_ptr, pitch, bpp, 20, cursor_y, "ERROR: NO MODULE IN ISO", 0xFF0000);
        } else {
            let m = &modules[0];
            vga::draw_string(fb_ptr, pitch, bpp, 20, cursor_y, "MODULE FOUND.", 0x00FF00);
            cursor_y += 20;

            // Applica offset HHDM per mappare dalla memoria fisica alla virtuale
            let raw_data   = m.data();
            let phys_addr  = raw_data.as_ptr() as u64;
            let mapped_ptr = (phys_addr + hhdm_offset) as *const u8;
            let data       = unsafe {
                core::slice::from_raw_parts(mapped_ptr, raw_data.len())
            };

            // Verifica firma GGUF prima di parsare
            if data.len() >= 4 && &data[0..4] == b"GGUF" {
                vga::draw_string(fb_ptr, pitch, bpp, 20, cursor_y, "GGUF SIGNATURE OK. PARSING...", 0x00FF00);
                cursor_y += 20;

                if let Some(model) = gguf::Gguf::new(data) {
                    vga::draw_string(fb_ptr, pitch, bpp, 20, cursor_y, "AI BRAIN ONLINE. READY.", 0x00FF00);
                    cursor_y += 20;

                    if let Some(gguf::GgufValue::String(name)) = model.find_metadata("general.name") {
                        vga::draw_string(fb_ptr, pitch, bpp, 20, cursor_y, name, 0xFFFF00);
                        cursor_y += 20;
                    } else {
                        vga::draw_string(fb_ptr, pitch, bpp, 20, cursor_y, "MODEL NAME NOT FOUND IN METADATA", 0xFFAA00);
                        cursor_y += 20;
                    }
                } else {
                    vga::draw_string(fb_ptr, pitch, bpp, 20, cursor_y, "ERROR: GGUF PARSE FAILED", 0xFF0000);
                    cursor_y += 20;
                }
            } else {
                vga::draw_string(fb_ptr, pitch, bpp, 20, cursor_y, "ERROR: NOT A VALID GGUF FILE", 0xFF0000);
                cursor_y += 20;
                // Mostra i primi 4 byte per debug
                if data.len() >= 4 {
                    let b = data[0];
                    unsafe { vga::draw_hex(fb_ptr, pitch, bpp, 20, cursor_y, b as u64, 0xFFFFFF); }
                    cursor_y += 20;
                }
            }
        }
    } else {
        vga::draw_string(fb_ptr, pitch, bpp, 20, cursor_y, "ERROR: MODULE REQUEST FAILED", 0xFF0000);
    }

    vga::draw_string(fb_ptr, pitch, bpp, 20, cursor_y + 10, "-- SYSTEM IDLE --", 0x888888);

    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Questo handler è sicuro: usa i globals salvati durante l'init.
    // Se FB_PTR è null (crash prima dell'init schermo), emergency_fill lo gestisce.
    vga::emergency_fill(0xFF0000);
    unsafe {
        if !vga::FB_PTR.is_null() {
            vga::draw_string(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP,
                             20, 20, "!!! KERNEL PANIC !!!", 0xFFFFFF);
            if let Some(loc) = info.location() {
                vga::draw_string(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP,
                                 20, 44, loc.file(), 0xFFFF00);
                vga::draw_hex(   vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP,
                                 20, 68, loc.line() as u64, 0xFFFFFF);
            }
        }
    }
    loop { unsafe { core::arch::asm!("hlt"); } }
}
