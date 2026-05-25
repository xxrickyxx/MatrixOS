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

// ── GUI-aware keyboard line reader ────────────────────────────────────────────

/// Blocking read into `buf` with live GUI input-box rendering.
/// Returns number of bytes typed (without the newline).
fn read_line_gui(kb: &mut keyboard::Keyboard, buf: &mut [u8]) -> usize {
    let mut len = 0usize;
    vga::draw_input_line(buf, len);   // show empty box + cursor initially

    loop {
        let sc = loop {
            if let Some(s) = kb.read_scancode() { break s; }
            unsafe { core::arch::asm!("pause"); }
        };

        if sc & 0x80 != 0 { continue; }   // ignore key-release events

        if let Some(ch) = kb.scancode_to_char(sc) {
            match ch {
                '\n' => {
                    vga::draw_input_line(&[], 0);   // clear input box
                    break;
                }
                '\x08' => {
                    if len > 0 {
                        len -= 1;
                        vga::draw_input_line(buf, len);
                    }
                }
                c => {
                    if len < buf.len() - 1 {
                        buf[len] = c as u8;
                        len += 1;
                        vga::draw_input_line(buf, len);
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

    // Init terminal to full-screen for the boot log
    vga::init_terminal(fb_ptr, pitch, bpp, fb_w, fb_h);
    delay();

    vga::term_puts("MATRIXOS v3.0  --  BARE-METAL LLM OS\n", vga::C_GREEN);
    vga::term_puts("Boot sequence starting...\n", vga::C_DIM);

    // ── Stage 1: hardware ────────────────────────────────────────────────
    unsafe { enable_sse(); core::arch::asm!("cli"); }
    vga::term_puts("[OK] SSE/AVX enabled\n", vga::C_GREEN);

    unsafe { idt::init(); }
    vga::term_puts("[OK] IDT loaded\n", vga::C_GREEN);

    if !BASE_REVISION.is_supported() {
        vga::term_puts("[FATAL] Limine revision not supported\n", vga::C_RED);
        loop { unsafe { core::arch::asm!("hlt"); } }
    }

    // ── Stage 2: heap ───────────────────────────────────────────────────
    allocator::init();
    vga::term_puts("[OK] Heap online (20 MB)\n", vga::C_GREEN);

    // ── Stage 3: HHDM ───────────────────────────────────────────────────
    match HHDM_REQUEST.response() {
        Some(_) => vga::term_puts("[OK] HHDM mapped\n", vga::C_GREEN),
        None    => vga::term_puts("[WARN] HHDM unavailable\n", vga::C_ORANGE),
    };

    // ── Stage 4: GGUF module ─────────────────────────────────────────────
    vga::term_puts("[..] Loading GGUF module...\n", vga::C_CYAN);

    let gguf_data: &[u8] = match MODULE_REQUEST.response() {
        None => {
            vga::term_puts("[FATAL] No module request response\n", vga::C_RED);
            loop { unsafe { core::arch::asm!("hlt"); } }
        }
        Some(res) => {
            let mods = res.modules();
            if mods.is_empty() {
                vga::term_puts("[FATAL] No module in ISO\n", vga::C_RED);
                loop { unsafe { core::arch::asm!("hlt"); } }
            }
            let m = &mods[0];
            m.data()  // Limine already provides HHDM-mapped virtual address
        }
    };

    // Verify GGUF magic
    if gguf_data.len() < 4 || &gguf_data[0..4] != b"GGUF" {
        vga::term_puts("[FATAL] Not a valid GGUF file\n", vga::C_RED);
        loop { unsafe { core::arch::asm!("hlt"); } }
    }

    let gguf = match gguf::Gguf::new(gguf_data) {
        None    => { vga::term_puts("[FATAL] GGUF parse failed\n", vga::C_RED); loop { unsafe { core::arch::asm!("hlt"); } } }
        Some(g) => g,
    };

    vga::term_puts("[OK] GGUF parsed. Tensors: ", vga::C_GREEN);
    vga::term_putu32(gguf.header.tensor_count as u32, vga::C_ORANGE);
    vga::term_puts("\n", 0);

    // ── Stage 5: Load config + tokenizer ─────────────────────────────────
    let cfg = model::load_config_from_gguf(&gguf);
    vga::term_puts("[OK] Config: d=", vga::C_GREEN);
    vga::term_putu32(cfg.d_model as u32, vga::C_ORANGE);
    vga::term_puts(" L=", vga::C_GREEN);
    vga::term_putu32(cfg.n_layers as u32, vga::C_ORANGE);
    vga::term_puts(" H=", vga::C_GREEN);
    vga::term_putu32(cfg.n_heads as u32, vga::C_ORANGE);
    vga::term_puts(" KV=", vga::C_GREEN);
    vga::term_putu32(cfg.n_kv_heads as u32, vga::C_ORANGE);
    vga::term_puts(" hd=", vga::C_GREEN);
    vga::term_putu32(cfg.head_dim as u32, vga::C_ORANGE);
    vga::term_puts(" ff=", vga::C_GREEN);
    vga::term_putu32(cfg.ffn_dim as u32, vga::C_ORANGE);
    vga::term_puts("\n", 0);

    let tokenizer = match tokenizer::Tokenizer::from_gguf(&gguf) {
        None    => { vga::term_puts("[WARN] Tokenizer not found\n", vga::C_ORANGE); None }
        Some(t) => {
            vga::term_puts("[OK] Tokenizer: vocab=", vga::C_GREEN);
            vga::term_putu32(t.count as u32, vga::C_ORANGE);
            vga::term_puts("\n", 0);
            Some(t)
        }
    };

    // ── Stage 6: Allocate KV cache + scratch ─────────────────────────────
    const MAX_SEQ: usize = 128;

    vga::term_puts("[..] Allocating KV cache...\n", vga::C_CYAN);
    let mut kv = model::KVCache::new(cfg.n_layers, cfg.n_kv_heads, cfg.head_dim, MAX_SEQ);
    vga::term_puts("[OK] KV cache ready\n", vga::C_GREEN);

    vga::term_puts("[..] Allocating scratch buffers...\n", vga::C_CYAN);
    let mut scratch = model::ForwardScratch::new(&cfg, MAX_SEQ);
    vga::term_puts("[OK] Scratch ready. Launching GUI...\n", vga::C_GREEN);

    // ── Stage 7: Switch to GUI ────────────────────────────────────────────
    // Gather model name from GGUF (or use default)
    let model_name: &str = if let Some(gguf::GgufValue::String(n)) = gguf.find_metadata("general.name") {
        n
    } else {
        "Qwen2.5-7B"
    };

    let vocab_count = tokenizer.as_ref().map(|t| t.count as u32).unwrap_or(cfg.vocab_size as u32);

    let sidebar = vga::SidebarInfo {
        model_name,
        quant:      "Q4_K_M",
        n_tensors:  gguf.header.tensor_count as u32,
        d_model:    cfg.d_model as u32,
        n_layers:   cfg.n_layers as u32,
        n_heads:    cfg.n_heads as u32,
        n_kv_heads: cfg.n_kv_heads as u32,
        head_dim:   cfg.head_dim as u32,
        ffn_dim:    cfg.ffn_dim as u32,
        vocab:      vocab_count,
    };
    vga::init_gui(&sidebar);

    // Welcome message in the chat area
    vga::term_puts("Welcome to MatrixOS — Bare-Metal AI\n", vga::C_CYAN);
    vga::term_puts("Type a question and press ENTER.\n", vga::C_DIM);
    vga::term_puts("(First response may take 1-2 min)\n\n", vga::C_DIM);

    // ── Stage 8: Interactive REPL ─────────────────────────────────────────
    let mut kb  = keyboard::Keyboard::new();
    let mut buf = [0u8; 256];
    let mut total_tokens: u32 = 0;

    loop {
        // Echo USER label into chat area, then read from GUI input box
        vga::term_puts("USER: ", vga::C_USER);
        let len = read_line_gui(&mut kb, &mut buf);
        if len == 0 { continue; }

        // Echo typed text + newline into chat area
        for i in 0..len { vga::term_putc(buf[i], vga::C_TEXT); }
        vga::term_putc(b'\n', 0);

        let user_text = core::str::from_utf8(&buf[..len]).unwrap_or("?");

        // AI response prefix
        vga::term_puts("AI:   ", vga::C_AI);

        // Tokenize using ChatML format
        let mut prompt_tokens: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
        if let Some(ref tok) = tokenizer {
            tok.build_chat_prompt(user_text, &mut prompt_tokens);
        } else {
            for &b in user_text.as_bytes() { prompt_tokens.push(b as u32); }
        }

        // Reset KV cache for new conversation turn
        for v in kv.k.iter_mut() { *v = 0.0; }
        for v in kv.v.iter_mut() { *v = 0.0; }

        // Generate response — tokens printed as they arrive
        let tok_ref = &tokenizer;
        let mut tok_buf = [0u8; 32];
        let mut turn_tokens: u32 = 0;

        model::generate(
            &gguf,
            &cfg,
            &mut kv,
            &mut scratch,
            &prompt_tokens,
            64, // max new tokens per turn
            |token_id| {
                turn_tokens += 1;
                total_tokens += 1;

                // Decode and print the token
                if let Some(ref t) = *tok_ref {
                    if let Some(s) = t.get_token(token_id as usize) {
                        let n = tokenizer::decode_token_to_buf(s, &mut tok_buf);
                        for i in 0..n {
                            vga::term_putc(tok_buf[i], vga::C_AI);
                        }
                    }
                } else {
                    vga::term_putu32(token_id, vga::C_AI);
                    vga::term_putc(b' ', 0);
                }

                // Update status bar token counter every 8 tokens to reduce overhead
                if total_tokens % 8 == 0 {
                    vga::update_status_tokens(total_tokens);
                }
            },
        );

        // Final token count update
        vga::update_status_tokens(total_tokens);
        vga::term_puts("\n\n", 0);
    }
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    vga::emergency_fill(0x3D0000);
    unsafe {
        if !vga::FB_PTR.is_null() {
            vga::draw_string(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP,
                             20, 20, "!!! KERNEL PANIC !!!", 0xFF4444);
            vga::draw_string(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP,
                             20, 44, "MatrixOS halted", 0xC9D1D9);
            if let Some(loc) = info.location() {
                vga::draw_string(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP,
                                 20, 68, loc.file(), 0xE3B341);
                vga::draw_hex(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP,
                              20, 92, loc.line() as u64, 0xFFFFFF);
            }
        }
    }
    loop { unsafe { core::arch::asm!("hlt"); } }
}
