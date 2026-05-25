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
pub mod bundle;
pub mod intent;
pub mod pci;
pub mod apps;

#[cfg(target_arch = "x86_64")]
pub mod memory;

use core::panic::PanicInfo;
use limine::request::{FramebufferRequest, ModulesRequest, HhdmRequest};
use limine::BaseRevision;

// ── Bare-metal `mem*` intrinsics ─────────────────────────────────────────────
// rustc/LLVM emit calls to memset/memcpy/memmove/memcmp for slice ops and
// `core::ptr::write_bytes`.  Without `compiler_builtins` mem feature on a
// no_std target these symbols are missing — the linker happily resolves them
// to address 0 (or wherever the .got points), which is what caused the
// "orange screen" boot hang.  Providing them explicitly fixes that.

#[no_mangle]
pub unsafe extern "C" fn memset(dst: *mut u8, val: i32, count: usize) -> *mut u8 {
    let v = val as u8;
    let mut i = 0usize;
    while i < count { *dst.add(i) = v; i += 1; }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, count: usize) -> *mut u8 {
    let mut i = 0usize;
    while i < count { *dst.add(i) = *src.add(i); i += 1; }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, count: usize) -> *mut u8 {
    if (dst as usize) < (src as usize) {
        let mut i = 0usize;
        while i < count { *dst.add(i) = *src.add(i); i += 1; }
    } else {
        let mut i = count;
        while i > 0 { i -= 1; *dst.add(i) = *src.add(i); }
    }
    dst
}

#[no_mangle]
pub unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, count: usize) -> i32 {
    let mut i = 0usize;
    while i < count {
        let a = *s1.add(i);
        let b = *s2.add(i);
        if a != b { return a as i32 - b as i32; }
        i += 1;
    }
    0
}

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

/// Result of read_line_gui: either a finished line (Enter), or a hotkey.
enum LineEvent {
    Line(usize),         // line len in buf
    Hotkey(char),
}

fn read_line_gui(kb: &mut keyboard::Keyboard, buf: &mut [u8]) -> LineEvent {
    let mut len = 0usize;
    vga::draw_input_line(buf, len);

    loop {
        let sc = loop {
            if let Some(s) = kb.read_scancode() { break s; }
            unsafe { core::arch::asm!("pause"); }
        };

        // Note: shift make/break IS handled inside scancode_to_char now.
        // Ignore non-shift releases.
        if sc & 0x80 != 0 && sc != 0xAA && sc != 0xB6 { continue; }

        if let Some(ch) = kb.scancode_to_char(sc) {
            match ch {
                '\n' => {
                    vga::draw_input_line(&[], 0);
                    return LineEvent::Line(len);
                }
                '\x08' => {
                    if len > 0 {
                        len -= 1;
                        vga::draw_input_line(buf, len);
                    }
                }
                // F-keys / TAB / ESC: bubble up as hotkeys
                c if (c as u32) < 0x20 && c != '\n' && c != '\x08'
                    || c == keyboard::KEY_TAB
                    || c == keyboard::KEY_ESC => {
                    vga::draw_input_line(&[], 0);
                    return LineEvent::Hotkey(c);
                }
                c => {
                    if len < buf.len() - 1 && (c as u32) < 128 {
                        buf[len] = c as u8;
                        len += 1;
                        vga::draw_input_line(buf, len);
                    }
                }
            }
        }
    }
}

// ── Helpers for switching active model slot ──────────────────────────────────

fn print_mode_switch(name: &str, color: u32) {
    vga::term_puts("\n[SYS] switched to ", vga::C_DIM);
    vga::term_puts(name, color);
    vga::term_puts("\n", 0);
}

/// Draw a 20×20 sentinel rectangle in the top-right area. Each stage uses a
/// different X so a row of colored squares forms as boot progresses. If the
/// row is short, we know which stage hung.
#[inline(never)]
unsafe fn stage_marker(fb: *mut u8, pitch: usize, bpp: usize, fb_w: usize,
                       stage: usize, r: u8, g: u8, b: u8) {
    if fb.is_null() { return; }
    let sx = fb_w.saturating_sub(20 * 14) + stage * 22;
    for py in 4..24usize {
        for px in 0..20usize {
            let off = py * pitch + (sx + px) * bpp;
            *fb.add(off)     = b;
            *fb.add(off + 1) = g;
            *fb.add(off + 2) = r;
        }
    }
}

// ── Slash-command dispatcher ─────────────────────────────────────────────────

fn handle_slash_command(
    line: &str,
    registry: &mut apps::Registry,
    max_new_chat: &mut usize,
    max_new_code: &mut usize,
    auto_pick: &mut bool,
    total_tokens: u32,
    tsc: u64,
    nic: &str, usb: &str, gpu: &str, storage: &str,
    model_name: &str,
    d_model: u32, n_layers: u32,
) {
    let line = line.trim();
    // Split into command + argument.
    let (cmd, arg) = match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], line[i..].trim_start()),
        None    => (line, ""),
    };

    match cmd {
        "/help" | "/?" => {
            vga::term_puts("Commands:\n", vga::C_CYAN);
            vga::term_puts("  /help              this list\n", vga::C_DIM);
            vga::term_puts("  /apps              list installed apps\n", vga::C_DIM);
            vga::term_puts("  /run <name>        execute an app\n", vga::C_DIM);
            vga::term_puts("  /sysinfo           print system stats\n", vga::C_DIM);
            vga::term_puts("  /set max_chat=N    chat response length (32..512)\n", vga::C_DIM);
            vga::term_puts("  /set max_code=N    code response length (32..640)\n", vga::C_DIM);
            vga::term_puts("  /set auto=on|off   intent auto-switching\n", vga::C_DIM);
            vga::term_puts("  /clear             clear chat scrollback\n", vga::C_DIM);
            vga::term_puts("Hotkeys: F1 chat · F2 code · TAB toggle auto · F5 clear\n", vga::C_DIM);
        }
        "/apps" => {
            let list = registry.list();
            vga::term_puts("Installed apps:\n", vga::C_CYAN);
            if list.is_empty() {
                vga::term_puts("  (none)\n", vga::C_DIM);
            }
            for a in list {
                vga::term_puts("  ", 0);
                vga::term_puts(&a.name, vga::C_GREEN);
                vga::term_puts("   [", vga::C_DIM);
                vga::term_puts(a.author.label(), vga::C_DIM);
                vga::term_puts(", ", vga::C_DIM);
                vga::term_putu32(a.recipe.len() as u32, vga::C_DIM);
                vga::term_puts(" cmds]\n", vga::C_DIM);
            }
        }
        "/run" => {
            if arg.is_empty() {
                vga::term_puts("usage: /run <app-name>\n", vga::C_RED);
                return;
            }
            let app = match registry.find(arg) {
                Some(a) => a.clone(),
                None => {
                    vga::term_puts("app not found: ", vga::C_RED);
                    vga::term_puts(arg, vga::C_RED);
                    vga::term_puts("\n", 0);
                    return;
                }
            };
            let ctx = apps::RuntimeCtx {
                total_tokens, tsc,
                nic, usb,
                model_name, d_model, n_layers,
            };
            vga::term_puts("--- running ", vga::C_DIM);
            vga::term_puts(&app.name, vga::C_GREEN);
            vga::term_puts(" ---\n", vga::C_DIM);
            apps::run_recipe(&app.recipe, &ctx);
            vga::term_puts("--- end ---\n", vga::C_DIM);
            // Silence unused-warning for the resources we may add later.
            let _ = gpu; let _ = storage;
        }
        "/sysinfo" => {
            vga::term_puts("MatrixOS v4.0\n", vga::C_CYAN);
            vga::term_puts("  model:   ", vga::C_DIM); vga::term_puts(model_name, vga::C_CYAN); vga::term_puts("\n", 0);
            vga::term_puts("  d_model: ", vga::C_DIM); vga::term_putu32(d_model, vga::C_ORANGE); vga::term_puts("\n", 0);
            vga::term_puts("  layers:  ", vga::C_DIM); vga::term_putu32(n_layers, vga::C_ORANGE); vga::term_puts("\n", 0);
            vga::term_puts("  tokens:  ", vga::C_DIM); vga::term_putu32(total_tokens, vga::C_CYAN); vga::term_puts("\n", 0);
            vga::term_puts("  NIC:     ", vga::C_DIM); vga::term_puts(nic, vga::C_CYAN); vga::term_puts("\n", 0);
            vga::term_puts("  USB:     ", vga::C_DIM); vga::term_puts(usb, vga::C_CYAN); vga::term_puts("\n", 0);
            vga::term_puts("  GPU:     ", vga::C_DIM); vga::term_puts(gpu, vga::C_CYAN); vga::term_puts("\n", 0);
            vga::term_puts("  Storage: ", vga::C_DIM); vga::term_puts(storage, vga::C_CYAN); vga::term_puts("\n", 0);
            vga::term_puts("  max_chat=", vga::C_DIM); vga::term_putu32(*max_new_chat as u32, vga::C_ORANGE);
            vga::term_puts(" max_code=", vga::C_DIM); vga::term_putu32(*max_new_code as u32, vga::C_ORANGE);
            vga::term_puts(" auto=", vga::C_DIM); vga::term_puts(if *auto_pick {"on"} else {"off"}, vga::C_GREEN);
            vga::term_puts("\n", 0);
        }
        "/set" => {
            // Format: /set key=value
            if let Some(eq) = arg.find('=') {
                let key = arg[..eq].trim();
                let val = arg[eq+1..].trim();
                match key {
                    "max_chat" => {
                        if let Some(n) = parse_u32_safe(val) {
                            let clamped = n.clamp(32, 512) as usize;
                            *max_new_chat = clamped;
                            vga::term_puts("[set] max_chat=", vga::C_GREEN);
                            vga::term_putu32(clamped as u32, vga::C_ORANGE);
                            vga::term_puts("\n", 0);
                        } else { vga::term_puts("bad number\n", vga::C_RED); }
                    }
                    "max_code" => {
                        if let Some(n) = parse_u32_safe(val) {
                            let clamped = n.clamp(32, 640) as usize;
                            *max_new_code = clamped;
                            vga::term_puts("[set] max_code=", vga::C_GREEN);
                            vga::term_putu32(clamped as u32, vga::C_ORANGE);
                            vga::term_puts("\n", 0);
                        } else { vga::term_puts("bad number\n", vga::C_RED); }
                    }
                    "auto" => {
                        let on = matches!(val, "on" | "ON" | "true" | "1");
                        *auto_pick = on;
                        vga::term_puts("[set] auto=", vga::C_GREEN);
                        vga::term_puts(if on {"on"} else {"off"}, vga::C_GREEN);
                        vga::term_puts("\n", 0);
                    }
                    _ => {
                        vga::term_puts("unknown key (max_chat/max_code/auto)\n", vga::C_RED);
                    }
                }
            } else {
                vga::term_puts("usage: /set key=value\n", vga::C_RED);
            }
        }
        "/clear" => {
            vga::clear_chat();
        }
        _ => {
            vga::term_puts("unknown command. try /help\n", vga::C_RED);
        }
    }
}

fn parse_u32_safe(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() { return None; }
    let mut n: u32 = 0;
    for b in s.bytes() {
        if !(b'0'..=b'9').contains(&b) { return None; }
        n = n.checked_mul(10)?.checked_add((b - b'0') as u32)?;
    }
    Some(n)
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
            // IMPORTANT: do NOT use core::ptr::write_bytes / 1D byte loop here.
            // LLVM compiles those to a memset call that this kernel has no
            // working implementation of (compiler_builtins memset fails on
            // bare-metal MB-sized clears, leaving an undefined screen — that
            // was the cause of the original "orange screen" boot hang).
            // The explicit 2D BGR loop below isn't recognised as memset and
            // is reliable.
            for y in 0..fb_h {
                for x in 0..fb_w {
                    let off = y * pitch + x * bpp;
                    unsafe {
                        *fb_ptr.add(off)     = 0x00; // B
                        *fb_ptr.add(off + 1) = 0x00; // G
                        *fb_ptr.add(off + 2) = 0x00; // R
                    }
                }
            }
        }
    }
    if fb_ptr.is_null() {
        loop { unsafe { core::arch::asm!("hlt"); } }
    }

    // Marker 0: framebuffer cleared OK (green) — proves color format
    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 0, 0x00, 0xFF, 0x00); }

    vga::init_terminal(fb_ptr, pitch, bpp, fb_w, fb_h);

    // Marker 1: init_terminal OK (cyan)
    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 1, 0x00, 0xFF, 0xFF); }
    delay();

    vga::term_puts("MATRIXOS v4.0 — DUAL-MODEL BARE-METAL LLM\n", vga::C_GREEN);
    vga::term_puts("Boot sequence...\n", vga::C_DIM);
    // Marker 2: first term_puts call returned (yellow) — proves draw_char works
    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 2, 0xFF, 0xFF, 0x00); }

    // ── Stage 1: hardware ────────────────────────────────────────────────
    unsafe { enable_sse(); core::arch::asm!("cli"); }
    vga::term_puts("[OK] SSE enabled\n", vga::C_GREEN);
    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 3, 0xFF, 0x80, 0x00); }

    unsafe { idt::init(); }
    vga::term_puts("[OK] IDT loaded\n", vga::C_GREEN);
    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 4, 0xFF, 0x00, 0xFF); }

    if !BASE_REVISION.is_supported() {
        vga::term_puts("[FATAL] Limine revision not supported\n", vga::C_RED);
        loop { unsafe { core::arch::asm!("hlt"); } }
    }

    // ── Stage 2: heap ───────────────────────────────────────────────────
    allocator::init();
    vga::term_puts("[OK] Heap online (64 MB)\n", vga::C_GREEN);
    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 5, 0x80, 0x80, 0xFF); }

    // ── Stage 3: HHDM ───────────────────────────────────────────────────
    let hhdm_offset: u64 = match HHDM_REQUEST.response() {
        Some(r) => {
            vga::term_puts("[OK] HHDM mapped\n", vga::C_GREEN);
            r.offset
        }
        None => {
            vga::term_puts("[WARN] HHDM unavailable\n", vga::C_ORANGE);
            0
        }
    };

    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 6, 0x40, 0xFF, 0x80); }

    // ── Stage 4: PCI scan ────────────────────────────────────────────────
    // hhdm_offset = 0 makes the MAC reader skip MMIO (boot-safe).
    let pci = pci::probe_summary(hhdm_offset);
    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 7, 0xFF, 0xC0, 0xC0); }
    vga::term_puts("[OK] PCI devices: ", vga::C_GREEN);
    vga::term_putu32(pci.total_devices, vga::C_ORANGE);
    vga::term_puts(", NICs: ", vga::C_GREEN);
    vga::term_putu32(pci.nic_count, vga::C_ORANGE);
    vga::term_puts(", USB: ", vga::C_GREEN);
    vga::term_putu32(pci.usb_count, vga::C_ORANGE);
    vga::term_puts("\n", 0);
    let nic_label: &str = if let Some(n) = pci.first_nic {
        let label = n.nic_model();
        vga::term_puts("[OK] NIC: ", vga::C_GREEN);
        vga::term_puts(label, vga::C_CYAN);
        // Show MAC if we got one (non-zero)
        let mac = &pci.nic_mac;
        if mac.iter().any(|&b| b != 0) {
            vga::term_puts("  MAC:", vga::C_DIM);
            for (i, &b) in mac.iter().enumerate() {
                if i > 0 { vga::term_putc(b':', vga::C_DIM); }
                vga::term_puthex8(b, vga::C_CYAN);
            }
        }
        vga::term_puts("\n", 0);
        label
    } else {
        "none"
    };
    let usb_label: &str = if let Some(u) = pci.first_usb {
        let label = u.usb_kind();
        vga::term_puts("[OK] USB: ", vga::C_GREEN);
        vga::term_puts(label, vga::C_CYAN);
        vga::term_puts("\n", 0);
        label
    } else {
        "none"
    };
    let gpu_label: &str = if let Some(g) = pci.first_gpu {
        let label = g.gpu_model();
        vga::term_puts("[OK] GPU: ", vga::C_GREEN);
        vga::term_puts(label, vga::C_CYAN);
        vga::term_puts("\n", 0);
        label
    } else {
        "none"
    };
    let storage_label: &str = if let Some(s) = pci.first_storage {
        let label = s.storage_kind();
        vga::term_puts("[OK] Storage: ", vga::C_GREEN);
        vga::term_puts(label, vga::C_CYAN);
        vga::term_puts(" (count=", vga::C_DIM);
        vga::term_putu32(pci.storage_count, vga::C_ORANGE);
        vga::term_puts(")\n", vga::C_DIM);
        label
    } else {
        "none"
    };

    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 8, 0xC0, 0xFF, 0xFF); }

    // ── Stage 5: Load Limine module ──────────────────────────────────────
    vga::term_puts("[..] Loading model module...\n", vga::C_CYAN);
    let mod_data: &[u8] = match MODULE_REQUEST.response() {
        None => {
            vga::term_puts("[FATAL] No module response\n", vga::C_RED);
            loop { unsafe { core::arch::asm!("hlt"); } }
        }
        Some(res) => {
            let mods = res.modules();
            if mods.is_empty() {
                vga::term_puts("[FATAL] No module in ISO\n", vga::C_RED);
                loop { unsafe { core::arch::asm!("hlt"); } }
            }
            mods[0].data()
        }
    };

    // Detect bundle vs single GGUF
    let (chat_bytes, chat_name_buf, code_bytes, code_name_buf, is_bundle) =
        if let Some(b) = bundle::Bundle::parse(mod_data) {
            vga::term_puts("[OK] Bundle detected: ", vga::C_GREEN);
            vga::term_putu32(b.count as u32, vga::C_ORANGE);
            vga::term_puts(" models\n", 0);
            let m0 = b.get(0).expect("model 0");
            let m1 = b.get(1).unwrap_or(m0);
            (m0.data, m0.name, m1.data, m1.name, true)
        } else {
            vga::term_puts("[OK] Single GGUF module\n", vga::C_GREEN);
            // Treat the same module as both slots
            (mod_data, "model", mod_data, "model", false)
        };

    if chat_bytes.len() < 4 || &chat_bytes[0..4] != b"GGUF" {
        vga::term_puts("[FATAL] Chat slot is not GGUF\n", vga::C_RED);
        loop { unsafe { core::arch::asm!("hlt"); } }
    }
    if code_bytes.len() < 4 || &code_bytes[0..4] != b"GGUF" {
        vga::term_puts("[FATAL] Code slot is not GGUF\n", vga::C_RED);
        loop { unsafe { core::arch::asm!("hlt"); } }
    }

    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 9, 0xC0, 0xC0, 0xFF); }
    let chat_gguf = gguf::Gguf::new(chat_bytes).expect("parse chat");
    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 10, 0xFF, 0xFF, 0xFF); }
    let code_gguf = gguf::Gguf::new(code_bytes).expect("parse code");
    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 11, 0x00, 0xC0, 0xC0); }

    let chat_cfg = model::load_config_from_gguf(&chat_gguf);
    let code_cfg = model::load_config_from_gguf(&code_gguf);
    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 12, 0xFF, 0x40, 0x40); }

    vga::term_puts("[OK] Chat ", vga::C_GREEN);
    vga::term_puts(chat_name_buf, vga::C_CYAN);
    vga::term_puts(": d=", vga::C_DIM);
    vga::term_putu32(chat_cfg.d_model as u32, vga::C_ORANGE);
    vga::term_puts(" L=", vga::C_DIM);
    vga::term_putu32(chat_cfg.n_layers as u32, vga::C_ORANGE);
    vga::term_puts("\n", 0);

    vga::term_puts("[OK] Code ", vga::C_GREEN);
    vga::term_puts(code_name_buf, vga::C_CYAN);
    vga::term_puts(": d=", vga::C_DIM);
    vga::term_putu32(code_cfg.d_model as u32, vga::C_ORANGE);
    vga::term_puts(" L=", vga::C_DIM);
    vga::term_putu32(code_cfg.n_layers as u32, vga::C_ORANGE);
    vga::term_puts("\n", 0);

    let chat_tok = tokenizer::Tokenizer::from_gguf(&chat_gguf);
    let code_tok = tokenizer::Tokenizer::from_gguf(&code_gguf);

    // ── Stage 6: Allocate KV caches + shared scratch ────────────────────
    const MAX_SEQ: usize = 128;
    vga::term_puts("[..] Allocating KV caches...\n", vga::C_CYAN);
    let mut chat_kv = model::KVCache::new(chat_cfg.n_layers, chat_cfg.n_kv_heads,
                                          chat_cfg.head_dim, MAX_SEQ);
    let mut code_kv = model::KVCache::new(code_cfg.n_layers, code_cfg.n_kv_heads,
                                          code_cfg.head_dim, MAX_SEQ);
    vga::term_puts("[OK] KV caches ready\n", vga::C_GREEN);

    // Build a max-config so the shared scratch fits both models
    let max_cfg = model::ModelConfig {
        d_model:    chat_cfg.d_model.max(code_cfg.d_model),
        n_layers:   chat_cfg.n_layers.max(code_cfg.n_layers),
        n_heads:    chat_cfg.n_heads.max(code_cfg.n_heads),
        n_kv_heads: chat_cfg.n_kv_heads.max(code_cfg.n_kv_heads),
        head_dim:   chat_cfg.head_dim.max(code_cfg.head_dim),
        ffn_dim:    chat_cfg.ffn_dim.max(code_cfg.ffn_dim),
        vocab_size: chat_cfg.vocab_size.max(code_cfg.vocab_size),
        rope_freq:  chat_cfg.rope_freq,
        bos:        chat_cfg.bos,
        eos:        chat_cfg.eos,
    };
    let mut scratch = model::ForwardScratch::new(&max_cfg, MAX_SEQ);
    vga::term_puts("[OK] Scratch ready. Launching GUI...\n", vga::C_GREEN);

    // ── Stage 7: GUI ──────────────────────────────────────────────────────
    let chat_display: &str = if is_bundle {
        chat_name_buf
    } else {
        // Use general.name from the GGUF
        if let Some(gguf::GgufValue::String(n)) = chat_gguf.find_metadata("general.name") { n }
        else { "qwen2.5" }
    };
    let code_display: &str = if is_bundle {
        code_name_buf
    } else { chat_display };

    let mut active_slot: u8 = 0;     // 0 = chat, 1 = code
    let mut active_cfg = chat_cfg;
    let mut active_tok = &chat_tok;
    let mut active_gguf = &chat_gguf;
    let mut active_name = chat_display;

    let sidebar = vga::SidebarInfo {
        model_name:  chat_display,
        quant:       "Q4_K_M",
        n_tensors:   chat_gguf.header.tensor_count as u32,
        d_model:     chat_cfg.d_model as u32,
        n_layers:    chat_cfg.n_layers as u32,
        n_heads:     chat_cfg.n_heads as u32,
        n_kv_heads:  chat_cfg.n_kv_heads as u32,
        head_dim:    chat_cfg.head_dim as u32,
        ffn_dim:     chat_cfg.ffn_dim as u32,
        vocab:       chat_tok.as_ref().map(|t| t.count as u32).unwrap_or(chat_cfg.vocab_size as u32),
        chat_name:   chat_display,
        code_name:   code_display,
        active_slot:  0,
        nic_info:     nic_label,
        usb_info:     usb_label,
        gpu_info:     gpu_label,
        storage_info: storage_label,
    };
    vga::init_gui(&sidebar);

    vga::term_puts("Welcome to MatrixOS — Dual-Model AI\n", vga::C_CYAN);
    vga::term_puts("F1 chat · F2 code · TAB auto-pick · F5 clear\n", vga::C_DIM);
    vga::term_puts("Slash commands: /help /apps /run <name> /sysinfo\n\n", vga::C_DIM);

    // ── Stage 8: REPL ─────────────────────────────────────────────────────
    let mut kb       = keyboard::Keyboard::new();
    let mut buf      = [0u8; 256];
    let mut total_tokens: u32 = 0;
    let mut auto_pick = true;   // TAB toggles

    // ── App registry (built-ins + AI-generated) ───────────────────────────
    let mut registry = apps::Registry::new();
    registry.install_builtins();

    // ── Settings the AI agents are allowed to change ──────────────────────
    let mut max_new_chat: usize = 192;   // generous; user wanted longer chats
    let mut max_new_code: usize = 320;

    loop {
        vga::term_puts("USER: ", vga::C_USER);
        let event = read_line_gui(&mut kb, &mut buf);
        let len = match event {
            LineEvent::Line(n) => n,
            LineEvent::Hotkey(k) => {
                match k {
                    keyboard::KEY_F1 => {
                        active_slot = 0;
                        active_cfg  = chat_cfg;
                        active_tok  = &chat_tok;
                        active_gguf = &chat_gguf;
                        active_name = chat_display;
                        vga::set_active_mode(0, chat_display, code_display);
                        print_mode_switch("CHAT", vga::C_CYAN);
                    }
                    keyboard::KEY_F2 => {
                        active_slot = 1;
                        active_cfg  = code_cfg;
                        active_tok  = &code_tok;
                        active_gguf = &code_gguf;
                        active_name = code_display;
                        vga::set_active_mode(1, chat_display, code_display);
                        print_mode_switch("CODE", vga::C_ORANGE);
                    }
                    keyboard::KEY_F5 => {
                        vga::clear_chat();
                        vga::term_puts("Welcome to MatrixOS — Dual-Model AI\n\n", vga::C_CYAN);
                    }
                    keyboard::KEY_TAB => {
                        auto_pick = !auto_pick;
                        vga::term_puts("\n[SYS] auto-pick ", vga::C_DIM);
                        vga::term_puts(if auto_pick {"ON"} else {"OFF"}, vga::C_GREEN);
                        vga::term_puts("\n", 0);
                    }
                    _ => {}
                }
                let _ = active_name;  // suppress unused-warning churn
                continue;
            }
        };
        if len == 0 { continue; }

        // Echo user line
        for i in 0..len { vga::term_putc(buf[i], vga::C_TEXT); }
        vga::term_putc(b'\n', 0);

        // ── Slash commands: handled in-kernel, no AI involved ─────────────
        if buf[0] == b'/' {
            let cmd_line = core::str::from_utf8(&buf[..len]).unwrap_or("/");
            handle_slash_command(
                cmd_line,
                &mut registry,
                &mut max_new_chat,
                &mut max_new_code,
                &mut auto_pick,
                total_tokens,
                math::rdtsc(),
                nic_label, usb_label, gpu_label, storage_label,
                active_name,
                active_cfg.d_model as u32,
                active_cfg.n_layers as u32,
            );
            vga::term_puts("\n", 0);
            continue;
        }

        // Auto-pick: maybe switch slot based on intent before responding
        if auto_pick {
            let (chosen, _score) = intent::classify(&buf[..len]);
            let want_slot: u8 = match chosen { intent::Mode::Chat => 0, intent::Mode::Code => 1 };
            if want_slot != active_slot {
                active_slot = want_slot;
                if want_slot == 0 {
                    active_cfg  = chat_cfg;
                    active_tok  = &chat_tok;
                    active_gguf = &chat_gguf;
                    active_name = chat_display;
                } else {
                    active_cfg  = code_cfg;
                    active_tok  = &code_tok;
                    active_gguf = &code_gguf;
                    active_name = code_display;
                }
                vga::set_active_mode(active_slot, chat_display, code_display);
                vga::term_puts("[auto→", vga::C_DIM);
                vga::term_puts(if active_slot==0 {"CHAT"} else {"CODE"},
                               if active_slot==0 {vga::C_CYAN} else {vga::C_ORANGE});
                vga::term_puts("] ", vga::C_DIM);
            }
        }

        let user_text = core::str::from_utf8(&buf[..len]).unwrap_or("?");

        // AI prefix (color depends on mode)
        let ai_color = if active_slot==0 { vga::C_AI } else { vga::C_ORANGE };
        vga::term_puts("AI:   ", ai_color);

        // Tokenize
        let mut prompt_tokens: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
        if let Some(ref tok) = *active_tok {
            tok.build_chat_prompt(user_text, &mut prompt_tokens);
        } else {
            for &b in user_text.as_bytes() { prompt_tokens.push(b as u32); }
        }

        // Reset the ACTIVE model's KV cache (not the other one — preserve its
        // unrelated context so future switches feel snappy)
        let kv_ref = if active_slot == 0 { &mut chat_kv } else { &mut code_kv };
        for v in kv_ref.k.iter_mut() { *v = 0.0; }
        for v in kv_ref.v.iter_mut() { *v = 0.0; }

        let mut tok_buf = [0u8; 32];
        let mut turn_tokens: u32 = 0;

        // TSC-based timing — rough tok/s estimate (assumes ~3 GHz CPU)
        let tsc_start = math::rdtsc();

        // User-tunable via /set max_chat=N or /set max_code=N
        let max_new: usize = if active_slot == 0 { max_new_chat } else { max_new_code };
        model::generate(
            active_gguf,
            &active_cfg,
            kv_ref,
            &mut scratch,
            &prompt_tokens,
            max_new,
            |token_id| {
                turn_tokens += 1;
                total_tokens += 1;
                if let Some(ref t) = *active_tok {
                    if let Some(s) = t.get_token(token_id as usize) {
                        let n = tokenizer::decode_token_to_buf(s, &mut tok_buf);
                        for i in 0..n {
                            vga::term_putc(tok_buf[i], ai_color);
                        }
                    }
                } else {
                    vga::term_putu32(token_id, ai_color);
                    vga::term_putc(b' ', 0);
                }
                if total_tokens % 4 == 0 {
                    vga::update_status_tokens(total_tokens, 0);
                }
            },
        );

        // Compute approximate tok/s: Δtsc / 3_000_000_000 ≈ seconds elapsed
        let tsc_end  = math::rdtsc();
        let delta    = tsc_end.wrapping_sub(tsc_start);
        // 3 GHz → 3e9 ticks/s; avoid division by zero
        let toks_per_sec: u32 = if delta > 0 && turn_tokens > 0 {
            // tok/s = turn_tokens / (delta / 3e9) = turn_tokens * 3e9 / delta
            // Use u64 to avoid overflow
            let num = (turn_tokens as u64).saturating_mul(3_000_000_000u64);
            (num / delta) as u32
        } else { 0 };

        vga::update_status_tokens(total_tokens, toks_per_sec);
        vga::term_puts("\n\n", 0);
    }
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // MAGENTA = panic (distinct from any normal-boot color)
    vga::emergency_fill(0xFF00FF);
    unsafe {
        if !vga::FB_PTR.is_null() {
            vga::draw_string(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP,
                             20, 20, "!!! KERNEL PANIC !!!", 0xFFFFFF);
            vga::draw_string(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP,
                             20, 44, "MatrixOS halted", 0xFFFFFF);
            if let Some(loc) = info.location() {
                // Bound the file-path draw to avoid the corruption pattern when
                // loc.file() returns a wild pointer (this is what produced the
                // "orange screen" before).
                let f = loc.file();
                let safe = if f.len() > 80 { &f[..80] } else { f };
                vga::draw_string(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP,
                                 20, 68, safe, 0xFFFFFF);
                vga::draw_hex(vga::FB_PTR, vga::FB_PITCH, vga::FB_BPP,
                              20, 92, loc.line() as u64, 0xFFFFFF);
            }
        }
    }
    loop { unsafe { core::arch::asm!("hlt"); } }
}
