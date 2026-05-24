#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate alloc;

pub mod allocator;
pub mod gguf;
pub mod vga;
pub mod keyboard;
pub mod idt;
pub mod tokenizer;
pub mod math;
pub mod model;
pub mod quant;

#[cfg(target_arch = "x86_64")]
pub mod memory;

use core::panic::PanicInfo;
use limine::request::{FramebufferRequest, ModulesRequest, HhdmRequest};
use limine::BaseRevision;

#[used] #[link_section = ".requests"]
static BASE_REVISION: BaseRevision = BaseRevision::new();
#[used] #[link_section = ".requests"]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();
#[used] #[link_section = ".requests"]
static MODULE_REQUEST: ModulesRequest = ModulesRequest::new();
#[used] #[link_section = ".requests"]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

// ── SSE enablement ────────────────────────────────────────────────────────────

unsafe fn enable_sse() {
    core::arch::asm!(
        "mov rax, cr0",
        "and eax, 0xFFFFFFFB",
        "or  eax, 0x2",
        "mov cr0, rax",
        "mov rax, cr4",
        "or  eax, 0x600",
        "mov cr4, rax",
        out("rax") _,
        options(nostack)
    );
}

fn delay() {
    for _ in 0..500_000 { unsafe { core::arch::asm!("nop"); } }
}

// ── Keyboard line reader ──────────────────────────────────────────────────────

/// Blocking read of one line (up to `max` chars); echoes each char.
/// Returns number of bytes written into `buf`.
fn read_line(kb: &mut keyboard::Keyboard, buf: &mut [u8]) -> usize {
    let mut len = 0usize;
    loop {
        // Poll keyboard (busy-wait)
        let sc = loop {
            if let Some(s) = kb.read_scancode() { break s; }
            unsafe { core::arch::asm!("pause"); }
        };

        // Ignore key-release (high bit set in basic scan set)
        if sc & 0x80 != 0 { continue; }

        if let Some(ch) = kb.scancode_to_char(sc) {
            match ch {
                '\n' => {
                    vga::term_putc(b'\n', 0xFFFFFF);
                    break;
                }
                '\x08' => {
                    if len > 0 {
                        len -= 1;
                        vga::term_putc(b'\x08', 0xFFFFFF);
                    }
                }
                c => {
                    if len < buf.len() - 1 {
                        buf[len] = c as u8;
                        len += 1;
                        vga::term_putc(c as u8, 0xFFFFFF);
                    }
                }
            }
        }
    }
    len
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // ── Stage 0: framebuffer ─────────────────────────────────────────────
    let mut fb_ptr:   *mut u8 = core::ptr::null_mut();
    let mut pitch:    usize   = 0;
    let mut bpp:      usize   = 0;
    let mut fb_w:     usize   = 0;
    let mut fb_h:     usize   = 0;

    if let Some(res) = FRAMEBUFFER_REQUEST.response() {
        if let Some(fb) = res.framebuffers().first() {
            fb_ptr = fb.address() as *mut u8;
            pitch  = fb.pitch as usize;
            bpp    = fb.bpp   as usize / 8;
            fb_w   = fb.width  as usize;
            fb_h   = fb.height as usize;
            let sz = pitch * fb_h;
            for i in 0..sz { unsafe { *fb_ptr.add(i) = 0; } }
        }
    }
    if fb_ptr.is_null() {
        loop { unsafe { core::arch::asm!("hlt"); } }
    }

    vga::init_terminal(fb_ptr, pitch, bpp, fb_w, fb_h);
    delay();

    vga::term_puts("MATRIXOS v3.0  --  BARE-METAL LLM OS\n", 0x00FF41);
    vga::term_puts("Boot sequence starting...\n", 0x888888);

    // ── Stage 1: hardware ────────────────────────────────────────────────
    unsafe { enable_sse(); core::arch::asm!("cli"); }
    vga::term_puts("[OK] SSE/AVX enabled\n", 0x00FF41);

    unsafe { idt::init(); }
    vga::term_puts("[OK] IDT loaded\n", 0x00FF41);

    if !BASE_REVISION.is_supported() {
        vga::term_puts("[FATAL] Limine revision not supported\n", 0xFF0000);
        loop { unsafe { core::arch::asm!("hlt"); } }
    }

    // ── Stage 2: heap ───────────────────────────────────────────────────
    allocator::init();
    vga::term_puts("[OK] Heap online (20 MB)\n", 0x00FF41);

    // ── Stage 3: HHDM ───────────────────────────────────────────────────
    match HHDM_REQUEST.response() {
        Some(_) => vga::term_puts("[OK] HHDM mapped\n", 0x00FF41),
        None    => vga::term_puts("[WARN] HHDM unavailable\n", 0xFFAA00),
    };

    // ── Stage 4: GGUF module ─────────────────────────────────────────────
    vga::term_puts("[..] Loading GGUF module...\n", 0x00FFFF);

    let gguf_data: &[u8] = match MODULE_REQUEST.response() {
        None => {
            vga::term_puts("[FATAL] No module request response\n", 0xFF0000);
            loop { unsafe { core::arch::asm!("hlt"); } }
        }
        Some(res) => {
            let mods = res.modules();
            if mods.is_empty() {
                vga::term_puts("[FATAL] No module in ISO\n", 0xFF0000);
                loop { unsafe { core::arch::asm!("hlt"); } }
            }
            let m = &mods[0];
            m.data()  // Limine already provides HHDM-mapped virtual address
        }
    };

    // Verify GGUF magic
    if gguf_data.len() < 4 || &gguf_data[0..4] != b"GGUF" {
        vga::term_puts("[FATAL] Not a valid GGUF file\n", 0xFF0000);
        loop { unsafe { core::arch::asm!("hlt"); } }
    }

    let gguf = match gguf::Gguf::new(gguf_data) {
        None    => { vga::term_puts("[FATAL] GGUF parse failed\n", 0xFF0000); loop { unsafe { core::arch::asm!("hlt"); } } }
        Some(g) => g,
    };

    vga::term_puts("[OK] GGUF parsed. Tensors: ", 0x00FF41);
    vga::term_putu32(gguf.header.tensor_count as u32, 0xFFFF00);
    vga::term_puts("\n", 0);

    if let Some(gguf::GgufValue::String(name)) = gguf.find_metadata("general.name") {
        vga::term_puts("[OK] Model: ", 0x00FF41);
        vga::term_puts(name, 0xFFFF00);
        vga::term_puts("\n", 0);
    }

    // ── Stage 5: Load config + tokenizer ─────────────────────────────────
    let cfg = model::load_config_from_gguf(&gguf);
    vga::term_puts("[OK] Config loaded. d_model=", 0x00FF41);
    vga::term_putu32(cfg.d_model as u32, 0xFFFF00);
    vga::term_puts(" layers=", 0x00FF41);
    vga::term_putu32(cfg.n_layers as u32, 0xFFFF00);
    vga::term_puts("\n", 0);

    let tokenizer = match tokenizer::Tokenizer::from_gguf(&gguf) {
        None    => { vga::term_puts("[WARN] Tokenizer not found in GGUF\n", 0xFFAA00); None }
        Some(t) => {
            vga::term_puts("[OK] Tokenizer loaded. Vocab=", 0x00FF41);
            vga::term_putu32(t.count as u32, 0xFFFF00);
            vga::term_puts("\n", 0);
            Some(t)
        }
    };

    // ── Stage 6: Allocate KV cache + scratch ─────────────────────────────
    const MAX_SEQ: usize = 128; // KV cache ~14 MB; ChatML prompt needs ~40+ slots

    vga::term_puts("[..] Allocating KV cache...\n", 0x00FFFF);
    let mut kv = model::KVCache::new(cfg.n_layers, cfg.n_kv_heads, cfg.head_dim, MAX_SEQ);
    vga::term_puts("[OK] KV cache allocated\n", 0x00FF41);

    vga::term_puts("[..] Allocating scratch buffers...\n", 0x00FFFF);
    let mut scratch = model::ForwardScratch::new(&cfg, MAX_SEQ);
    vga::term_puts("[OK] Scratch ready\n", 0x00FF41);

    // ── Stage 7: Interactive REPL ─────────────────────────────────────────
    vga::term_puts("\n=== MATRIXOS AI TERMINAL ===\n", 0x00FF41);
    vga::term_puts("Type your question and press ENTER.\n", 0x888888);
    vga::term_puts("(Generation may be slow on bare metal)\n\n", 0x888888);

    let mut kb  = keyboard::Keyboard::new();
    let mut buf = [0u8; 256];

    loop {
        vga::term_puts("USER> ", 0x00FFFF);
        let len = read_line(&mut kb, &mut buf);
        if len == 0 { continue; }

        let user_text = core::str::from_utf8(&buf[..len]).unwrap_or("?");

        // Tokenize
        vga::term_puts("AI>   ", 0x00FF41);

        let mut prompt_tokens: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
        if let Some(ref tok) = tokenizer {
            tok.build_chat_prompt(user_text, &mut prompt_tokens);
        } else {
            // Fallback: byte-level tokens
            for &b in user_text.as_bytes() { prompt_tokens.push(b as u32); }
        }

        // Reset KV cache for new conversation turn
        for v in kv.k.iter_mut() { *v = 0.0; }
        for v in kv.v.iter_mut() { *v = 0.0; }

        // Generate response — tokens printed as they arrive
        let tok_ref = &tokenizer;
        let mut tok_buf = [0u8; 32];
        model::generate(
            &gguf,
            &cfg,
            &mut kv,
            &mut scratch,
            &prompt_tokens,
            64, // max new tokens
            |token_id| {
                if let Some(ref t) = *tok_ref {
                    if let Some(s) = t.get_token(token_id as usize) {
                        let n = tokenizer::decode_token_to_buf(s, &mut tok_buf);
                        for i in 0..n {
                            vga::term_putc(tok_buf[i], 0x00FF41);
                        }
                    }
                } else {
                    // Fallback: print token id
                    vga::term_putu32(token_id, 0x00FF41);
                    vga::term_putc(b' ', 0x00FF41);
                }
            },
        );
        vga::term_puts("\n\n", 0);
    }
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    vga::emergency_fill(0xFF0000);
    unsafe {
        if !vga::FB_PTR.is_null() {
            vga::draw_string(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP, 20, 20,
                             "!!! KERNEL PANIC !!!", 0xFFFFFF);
            if let Some(loc) = info.location() {
                vga::draw_string(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP, 20, 44,
                                 loc.file(), 0xFFFF00);
                vga::draw_hex(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP, 20, 68,
                              loc.line() as u64, 0xFFFFFF);
            }
        }
    }
    loop { unsafe { core::arch::asm!("hlt"); } }
}
