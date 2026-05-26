#![no_std]
#![no_main]
#![allow(static_mut_refs)]
#![feature(abi_x86_interrupt)]

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
pub mod ata;
pub mod state;
pub mod net;
pub mod bootstub;
pub mod irq;
pub mod ahci;
pub mod fs;
pub mod mouse;
pub mod audio;
pub mod rng;
pub mod planner;
pub mod executor;

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

/// HHDM offset cached after boot — read by slash commands that need to
/// translate physical addresses (e.g. NIC MMIO BARs) into kernel virtual.
static mut HHDM_OFFSET: u64 = 0;

/// Our IPv4 address — manually configurable via /net ip <a.b.c.d>.
/// Default 10.0.2.15 because that's what VirtualBox NAT hands out by DHCP
/// (we don't speak DHCP yet, so the user can /net ip-set if needed).
static mut OUR_IP: [u8; 4] = [10, 0, 2, 15];

/// DNS server (set by /dhcp ACK or /net dns).  Default = VBox NAT.
static mut DNS_IP: [u8; 4] = [10, 0, 2, 3];

/// Gateway (set by /dhcp ACK).  Default = VBox NAT.
static mut GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];

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

/// Command-history ring buffer (16 entries × 256 bytes).  Shared by
/// every read_line_gui call so Up/Down recall the last N commands.
const HIST_CAP: usize = 16;
const HIST_LINE: usize = 256;
static mut HIST: [[u8; HIST_LINE]; HIST_CAP] = [[0; HIST_LINE]; HIST_CAP];
static mut HIST_LEN: [usize; HIST_CAP] = [0; HIST_CAP];
static mut HIST_HEAD: usize = 0;   // next slot to write into
static mut HIST_COUNT: usize = 0;  // number of valid entries (≤ HIST_CAP)

fn hist_push(line: &[u8]) {
    if line.is_empty() { return; }
    unsafe {
        let slot = HIST_HEAD;
        let n = line.len().min(HIST_LINE);
        HIST[slot][..n].copy_from_slice(&line[..n]);
        HIST_LEN[slot] = n;
        HIST_HEAD = (HIST_HEAD + 1) % HIST_CAP;
        if HIST_COUNT < HIST_CAP { HIST_COUNT += 1; }
    }
}

/// Get history entry `offset` back (1 = most recent).  Returns None if out
/// of range.
fn hist_get(offset: usize) -> Option<&'static [u8]> {
    unsafe {
        if offset == 0 || offset > HIST_COUNT { return None; }
        // Walk backwards from HEAD by `offset` slots.
        let slot = (HIST_HEAD + HIST_CAP - offset) % HIST_CAP;
        let n = HIST_LEN[slot];
        Some(&HIST[slot][..n])
    }
}

fn read_line_gui(kb: &mut keyboard::Keyboard, buf: &mut [u8]) -> LineEvent {
    let mut len = 0usize;
    let mut dirty = true;
    let mut hist_pos: usize = 0;   // 0 = current (editable), N = history offset

    loop {
        if dirty {
            vga::draw_input_line(buf, len);
            dirty = false;
        }

        // Drain ALL pending scancodes in a tight loop before redrawing.
        // Without this, the redraw between keys (~10 ms of pixel pushing)
        // makes us miss bytes from the PS/2 controller's 1-byte FIFO when
        // the host types fast (computer-use, paste, etc.).
        let mut produced_something = false;
        while let Some(sc) = kb.read_scancode() {
            produced_something = true;
            // Break codes: drop everything except shift make/break and the
            // 0xE0 extended-key prefix.
            if sc & 0x80 != 0 && sc != 0xAA && sc != 0xB6 && sc != 0xE0 { continue; }
            if let Some(ch) = kb.scancode_to_char(sc) {
                match ch {
                    '\n' => {
                        vga::draw_input_line(&[], 0);
                        // Push into history (skip duplicates of most recent).
                        let line = &buf[..len];
                        let dup = hist_get(1).map(|prev| prev == line).unwrap_or(false);
                        if !dup { hist_push(line); }
                        return LineEvent::Line(len);
                    }
                    '\x08' => {
                        if len > 0 { len -= 1; dirty = true; }
                    }
                    keyboard::KEY_UP => {
                        let want = hist_pos + 1;
                        if let Some(prev) = hist_get(want) {
                            let n = prev.len().min(buf.len() - 1);
                            buf[..n].copy_from_slice(&prev[..n]);
                            len = n;
                            hist_pos = want;
                            dirty = true;
                        }
                    }
                    keyboard::KEY_DOWN => {
                        if hist_pos > 1 {
                            hist_pos -= 1;
                            if let Some(prev) = hist_get(hist_pos) {
                                let n = prev.len().min(buf.len() - 1);
                                buf[..n].copy_from_slice(&prev[..n]);
                                len = n;
                                dirty = true;
                            }
                        } else if hist_pos == 1 {
                            // Snap back to empty
                            len = 0;
                            hist_pos = 0;
                            dirty = true;
                        }
                    }
                    keyboard::KEY_LEFT | keyboard::KEY_RIGHT => {
                        // No cursor-move yet; ignore.
                    }
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
                            hist_pos = 0;   // editing breaks history navigation
                            dirty = true;
                        }
                    }
                }
            }
        }
        if !produced_something {
            // Refresh mouse cursor + live clock while idle (each call is
            // O(no-op) unless the underlying state has actually changed).
            mouse::draw_cursor(vga::C_BG);
            let ms = irq::tick_ms();
            vga::update_status_clock((ms / 1000) as u32);

            // Blink the input-line cursor at ~2 Hz.  Only repaint when the
            // visibility state actually changes — cheap.
            let blink_on = (ms / 500) % 2 == 0;
            static mut LAST_BLINK: bool = true;
            unsafe {
                if blink_on != LAST_BLINK {
                    LAST_BLINK = blink_on;
                    vga::draw_input_line_blink(buf, len, blink_on);
                }
            }

            unsafe { core::arch::asm!("pause"); }
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
    boot_tsc: u64,
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
            vga::term_puts("  /create <name> <desc>   AI builds the app\n", vga::C_DIM);
            vga::term_puts("  /show <name>       print recipe source\n", vga::C_DIM);
            vga::term_puts("  /delete <name>     remove a non-builtin app\n", vga::C_DIM);
            vga::term_puts("  /disks             list controllers + IDENTIFY drives\n", vga::C_DIM);
            vga::term_puts("  /install <slot>             IDE install (dry-run)\n", vga::C_DIM);
            vga::term_puts("  /install ahci <port>        SATA install (dry-run)\n", vga::C_DIM);
            vga::term_puts("  /install ... confirm        actually write\n", vga::C_DIM);
            vga::term_puts("  /sysinfo           print system stats\n", vga::C_DIM);
            vga::term_puts("  /set max_chat=N    chat response length (32..512)\n", vga::C_DIM);
            vga::term_puts("  /set max_code=N    code response length (32..640)\n", vga::C_DIM);
            vga::term_puts("  /set auto=on|off   intent auto-switching\n", vga::C_DIM);
            vga::term_puts("  /save              persist apps + settings to disk\n", vga::C_DIM);
            vga::term_puts("  /load              read saved state from disk\n", vga::C_DIM);
            vga::term_puts("  /fs ls|save|cat|rm|format   tiny on-disk filesystem\n", vga::C_DIM);
            vga::term_puts("  /net               NIC MAC + link state\n", vga::C_DIM);
            vga::term_puts("  /net reset         soft-reset the NIC\n", vga::C_DIM);
            vga::term_puts("  /net arp <ip>      send a broadcast ARP request\n", vga::C_DIM);
            vga::term_puts("  /net ip <a.b.c.d>  set our IPv4 address\n", vga::C_DIM);
            vga::term_puts("  /ping <ip>         ARP + ICMP-echo a host\n", vga::C_DIM);
            vga::term_puts("  /dhcp              DHCP DISCOVER+REQUEST, auto-config IP\n", vga::C_DIM);
            vga::term_puts("  /tcp <ip> <port> <text>   TCP handshake + send + read\n", vga::C_DIM);
            vga::term_puts("  /resolve <host>    DNS A-record lookup\n", vga::C_DIM);
            vga::term_puts("  /http <host> [path]   DNS + TCP GET + print body\n", vga::C_DIM);
            vga::term_puts("  /meminfo           heap used / free\n", vga::C_DIM);
            vga::term_puts("  /uptime            seconds since boot (TSC)\n", vga::C_DIM);
            vga::term_puts("  /clear             clear chat scrollback\n", vga::C_DIM);
            vga::term_puts("  /plan <natural-language>   AI emits JSON plan + executes\n", vga::C_DIM);
            vga::term_puts("  /beep [freq] [ms]  PC-speaker tone\n", vga::C_DIM);
            vga::term_puts("  /rand              random 64-bit value\n", vga::C_DIM);
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
        "/disks" => {
            // Step 1 — every PCI storage controller (IDE / SATA / NVMe / RAID).
            vga::term_puts("PCI storage controllers:\n", vga::C_CYAN);
            let mut ctl_count = 0u32;
            let mut ahci_count = 0u32;
            let mut nvme_count = 0u32;
            pci::enumerate(|d| {
                if d.is_storage() {
                    ctl_count += 1;
                    vga::term_puts("  ", 0);
                    vga::term_puts(d.storage_kind(), vga::C_GREEN);
                    vga::term_puts("   ", vga::C_DIM);
                    vga::term_puthex8((d.vendor_id >> 8) as u8, vga::C_ORANGE);
                    vga::term_puthex8(d.vendor_id as u8, vga::C_ORANGE);
                    vga::term_puts(":", vga::C_DIM);
                    vga::term_puthex8((d.device_id >> 8) as u8, vga::C_ORANGE);
                    vga::term_puthex8(d.device_id as u8, vga::C_ORANGE);
                    if d.subclass == 0x06 { ahci_count += 1; vga::term_puts("  (AHCI)", vga::C_ORANGE); }
                    if d.subclass == 0x08 { nvme_count += 1; vga::term_puts("  (NVMe)", vga::C_ORANGE); }
                    vga::term_puts("\n", 0);
                }
            });
            if ctl_count == 0 {
                vga::term_puts("  (none)\n", vga::C_DIM);
            }
            if ahci_count > 0 || nvme_count > 0 {
                vga::term_puts("  note: AHCI / NVMe controllers are NOT yet driven —\n", vga::C_ORANGE);
                vga::term_puts("  drives behind them won't appear below.  Use the legacy\n", vga::C_DIM);
                vga::term_puts("  IDE controller in your VM settings.\n", vga::C_DIM);
            }

            // Step 2 — actual drives via ATA IDENTIFY on the legacy IDE
            // controllers (primary 0x1F0, secondary 0x170, master+slave each).
            vga::term_puts("\nATA / IDE drives:\n", vga::C_CYAN);
            let probe = ata::probe_all();
            let mut idx = 0u32;
            for (slot, d) in probe.drives.iter().enumerate() {
                if let Some(info) = d {
                    vga::term_puts("  [", vga::C_DIM);
                    vga::term_putu32(slot as u32, vga::C_ORANGE);
                    vga::term_puts("] ", 0);
                    vga::term_puts(info.addr.label(), vga::C_GREEN);
                    vga::term_puts("  ", 0);
                    vga::term_puts(info.model_str(), vga::C_TEXT);
                    if info.is_atapi { vga::term_puts(" [ATAPI]", vga::C_DIM); }
                    vga::term_puts("\n      sectors: ", vga::C_DIM);
                    vga::term_putu32(info.total_sectors() as u32, vga::C_ORANGE);
                    vga::term_puts("  size: ", vga::C_DIM);
                    let gb_x10 = info.capacity_gb_x10();
                    if gb_x10 >= 10 {
                        vga::term_putu32(gb_x10 / 10, vga::C_ORANGE);
                        vga::term_putc(b'.', vga::C_DIM);
                        vga::term_putu32(gb_x10 % 10, vga::C_ORANGE);
                        vga::term_puts(" GB", vga::C_DIM);
                    } else {
                        vga::term_putu32(info.capacity_mb(), vga::C_ORANGE);
                        vga::term_puts(" MB", vga::C_DIM);
                    }
                    vga::term_puts("  LBA: ", vga::C_DIM);
                    vga::term_puts(if info.sectors_48 > 0 {"48-bit"} else {"28-bit"}, vga::C_GREEN);
                    vga::term_puts("\n", 0);
                    idx += 1;
                }
            }
            if idx == 0 {
                vga::term_puts("  (no ATA drives responded to IDENTIFY)\n", vga::C_DIM);
            }

            // Step 3 — AHCI / SATA drives via the new ahci.rs driver.
            let ahci_probe = ahci::probe(unsafe { HHDM_OFFSET });
            if ahci_probe.controller_present {
                vga::term_puts("\nSATA / AHCI drives:\n", vga::C_CYAN);
                if ahci_probe.drives.is_empty() {
                    vga::term_puts("  controller present, no drives responded\n", vga::C_DIM);
                } else {
                    for d in &ahci_probe.drives {
                        vga::term_puts("  port ", vga::C_DIM);
                        vga::term_putu32(d.port as u32, vga::C_ORANGE);
                        vga::term_puts("  ", 0);
                        vga::term_puts(d.model_str(), vga::C_TEXT);
                        if d.is_atapi { vga::term_puts(" [ATAPI]", vga::C_DIM); }
                        vga::term_puts("\n      sectors: ", vga::C_DIM);
                        vga::term_putu32(d.sectors_48 as u32, vga::C_ORANGE);
                        vga::term_puts("  size: ", vga::C_DIM);
                        let gb10 = d.capacity_gb_x10();
                        if gb10 >= 10 {
                            vga::term_putu32(gb10 / 10, vga::C_ORANGE);
                            vga::term_putc(b'.', vga::C_DIM);
                            vga::term_putu32(gb10 % 10, vga::C_ORANGE);
                            vga::term_puts(" GB\n", vga::C_DIM);
                        } else {
                            vga::term_putu32(d.capacity_mb(), vga::C_ORANGE);
                            vga::term_puts(" MB\n", vga::C_DIM);
                        }
                    }
                }
            }

            if idx == 0 && ahci_probe.drives.is_empty() {
                vga::term_puts("\nAttach an IDE or SATA disk in VM settings.\n", vga::C_DIM);
            } else if idx > 0 {
                vga::term_puts("Use /install <slot> to install (IDE only for now).\n", vga::C_DIM);
            }
        }
        "/install" => {
            // /install is intercepted earlier in the REPL so it can access
            // the in-RAM module bundle.  We should never get here.
            vga::term_puts("(internal: /install dispatcher fallthrough)\n", vga::C_RED);
        }
        "/sysinfo" => {
            vga::term_puts("MatrixOS v4.0\n", vga::C_CYAN);

            // CPU info from CPUID
            let mut vendor = [0u8; 12];
            math::cpu_vendor(&mut vendor);
            let mut brand  = [0u8; 48];
            let brand_len  = math::cpu_brand(&mut brand);
            vga::term_puts("  CPU:     ", vga::C_DIM);
            if brand_len > 0 {
                if let Ok(s) = core::str::from_utf8(&brand[..brand_len]) {
                    vga::term_puts(s.trim(), vga::C_CYAN);
                }
            } else if let Ok(s) = core::str::from_utf8(&vendor) {
                vga::term_puts(s, vga::C_CYAN);
            }
            vga::term_puts("\n", 0);
            vga::term_puts("  ISA:     ", vga::C_DIM);
            vga::term_puts(if math::avx2_available() {"AVX2+FMA"} else {"SSE3"}, vga::C_GREEN);
            vga::term_puts("  logical=", vga::C_DIM);
            vga::term_putu32(math::cpu_logical_count(), vga::C_ORANGE);
            vga::term_puts("\n", 0);

            vga::term_puts("  model:   ", vga::C_DIM); vga::term_puts(model_name, vga::C_CYAN); vga::term_puts("\n", 0);
            vga::term_puts("  d_model: ", vga::C_DIM); vga::term_putu32(d_model, vga::C_ORANGE); vga::term_puts("\n", 0);
            vga::term_puts("  layers:  ", vga::C_DIM); vga::term_putu32(n_layers, vga::C_ORANGE); vga::term_puts("\n", 0);
            vga::term_puts("  tokens:  ", vga::C_DIM); vga::term_putu32(total_tokens, vga::C_CYAN); vga::term_puts("\n", 0);
            vga::term_puts("  heap:    ", vga::C_DIM);
            vga::term_putu32(unsafe { (allocator::HEAP_BYTES / (1024*1024)) as u32 }, vga::C_ORANGE);
            vga::term_puts(" MB\n", vga::C_DIM);
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
        "/net" => {
            // Parsing the sub-action.  Supported:
            //   /net              report MAC, link, basic info
            //   /net reset        CTRL.RST + redo basic init
            //   /net arp <ip>     bring up TX/RX rings, send 1 broadcast ARP
            //   /net stats        re-show last driver TX/RX counters
            let hhdm = unsafe { HHDM_OFFSET };
            let (sub, sub_arg) = match arg.find(char::is_whitespace) {
                Some(i) => (&arg[..i], arg[i..].trim_start()),
                None    => (arg, ""),
            };

            let mut nic = match net::probe(hhdm) {
                Some(n) => n,
                None => {
                    vga::term_puts("No e1000-class NIC found, or HHDM unavailable.\n", vga::C_RED);
                    return;
                }
            };

            // Always print the basic header.
            vga::term_puts("NIC: ", vga::C_CYAN);
            vga::term_puts(nic.addr.nic_model(), vga::C_GREEN);
            vga::term_puts("\n  MAC:  ", vga::C_DIM);
            for (i, &b) in nic.mac.iter().enumerate() {
                if i > 0 { vga::term_putc(b':', vga::C_DIM); }
                vga::term_puthex8(b, vga::C_CYAN);
            }
            vga::term_puts("\n  link: ", vga::C_DIM);
            if nic.link_up {
                vga::term_puts("UP  ", vga::C_GREEN);
                vga::term_putu32(nic.speed_mbps, vga::C_ORANGE);
                vga::term_puts(" Mbps", vga::C_DIM);
                vga::term_puts(if nic.full_duplex {" full-dup\n"} else {" half-dup\n"}, vga::C_DIM);
            } else {
                vga::term_puts("DOWN\n", vga::C_RED);
            }

            match sub {
                "" => {
                    vga::term_puts("  hint: /net reset | /net arp <ip>\n", vga::C_DIM);
                }
                "reset" => {
                    vga::term_puts("  CTRL.RST issued\n", vga::C_CYAN);
                    unsafe { net::soft_reset(&nic); }
                }
                "ip" => {
                    match parse_ipv4(sub_arg) {
                        Some(ip) => {
                            unsafe { OUR_IP = ip; }
                            vga::term_puts("  OUR_IP = ", vga::C_GREEN);
                            for (i, &b) in ip.iter().enumerate() {
                                if i > 0 { vga::term_putc(b'.', vga::C_DIM); }
                                vga::term_putu32(b as u32, vga::C_CYAN);
                            }
                            vga::term_puts("\n", 0);
                        }
                        None => { vga::term_puts("usage: /net ip <a.b.c.d>\n", vga::C_RED); }
                    }
                }
                "arp" => {
                    // Parse "a.b.c.d"
                    let ip = match parse_ipv4(sub_arg) {
                        Some(ip) => ip,
                        None => {
                            vga::term_puts("usage: /net arp <a.b.c.d>\n", vga::C_RED);
                            return;
                        }
                    };
                    vga::term_puts("  bringing up driver (TX+RX rings)...\n", vga::C_CYAN);
                    let mut drv = net::Driver::init(nic, hhdm);
                    vga::term_puts("  sending ARP-who-has ", vga::C_CYAN);
                    for (i, &b) in ip.iter().enumerate() {
                        if i > 0 { vga::term_putc(b'.', vga::C_DIM); }
                        vga::term_putu32(b as u32, vga::C_ORANGE);
                    }
                    vga::term_puts(" ...\n", vga::C_CYAN);
                    let sender_ip = [0u8, 0, 0, 0];   // we have no IP yet
                    match drv.send_arp(sender_ip, ip) {
                        Ok(()) => {
                            vga::term_puts("  TX OK (1 frame sent)\n", vga::C_GREEN);
                            // Poll a few iterations for any RX frame.
                            vga::term_puts("  polling RX for 100 ms-ish...\n", vga::C_DIM);
                            let mut rx_seen = 0u32;
                            for _ in 0..200_000u32 {
                                if let Some((idx, len)) = drv.poll_rx() {
                                    rx_seen += 1;
                                    vga::term_puts("  RX[", vga::C_GREEN);
                                    vga::term_putu32(idx as u32, vga::C_ORANGE);
                                    vga::term_puts("] ", vga::C_DIM);
                                    vga::term_putu32(len as u32, vga::C_GREEN);
                                    vga::term_puts(" bytes  type=0x", vga::C_DIM);
                                    let frame = drv.rx_frame(idx);
                                    if frame.len() >= 14 {
                                        vga::term_puthex8(frame[12], vga::C_ORANGE);
                                        vga::term_puthex8(frame[13], vga::C_ORANGE);
                                    }
                                    vga::term_puts("\n", 0);
                                    if rx_seen >= 4 { break; }
                                }
                                unsafe { core::arch::asm!("pause"); }
                            }
                            if rx_seen == 0 {
                                vga::term_puts("  no frames received (VM NAT may not respond to ARP)\n", vga::C_DIM);
                            }
                            vga::term_puts("  stats: TX=", vga::C_DIM);
                            vga::term_putu32(drv.tx_sent, vga::C_GREEN);
                            vga::term_puts(" RX=", vga::C_DIM);
                            vga::term_putu32(drv.rx_recv, vga::C_GREEN);
                            vga::term_puts("\n", 0);
                        }
                        Err(e) => {
                            vga::term_puts("  TX error: ", vga::C_RED);
                            vga::term_puts(e, vga::C_RED);
                            vga::term_puts("\n", 0);
                        }
                    }
                }
                _ => {
                    vga::term_puts("unknown /net subcommand. try: /net | /net reset | /net arp <ip>\n", vga::C_RED);
                }
            }
        }
        "/tcp" => {
            // Format: /tcp <ip> <port> <text>
            let (ip_str, rest) = match arg.find(char::is_whitespace) {
                Some(i) => (&arg[..i], arg[i..].trim_start()),
                None => { vga::term_puts("usage: /tcp <ip> <port> <text>\n", vga::C_RED); return; }
            };
            let (port_str, text) = match rest.find(char::is_whitespace) {
                Some(i) => (&rest[..i], rest[i..].trim_start()),
                None => (rest, ""),
            };
            let dst_ip = match parse_ipv4(ip_str) {
                Some(ip) => ip,
                None => { vga::term_puts("bad IP\n", vga::C_RED); return; }
            };
            let dst_port = match parse_u32_safe(port_str) {
                Some(n) if n < 65536 => n as u16,
                _ => { vga::term_puts("bad port\n", vga::C_RED); return; }
            };

            let hhdm = unsafe { HHDM_OFFSET };
            let nic = match net::probe(hhdm) {
                Some(n) => n,
                None => { vga::term_puts("no NIC\n", vga::C_RED); return; }
            };
            let mut drv = net::Driver::init(nic, hhdm);
            let mut cache = net::ArpCache::new();
            let our_ip = unsafe { OUR_IP };

            // ARP for target (or gateway if not same subnet — we assume same subnet)
            vga::term_puts("[tcp] arp ", vga::C_DIM); print_ipv4(dst_ip); vga::term_puts("...\n", vga::C_DIM);
            if drv.send_arp(our_ip, dst_ip).is_err() {
                vga::term_puts("[tcp] ARP TX failed\n", vga::C_RED); return;
            }
            let mut dst_mac: Option<[u8; 6]> = None;
            for _ in 0..400_000u32 {
                if let Some((idx, _)) = drv.poll_rx() {
                    let f = drv.rx_frame(idx);
                    if let Some(reply) = net::process_arp(f, &mut cache, drv.nic.mac, our_ip) {
                        let _ = drv.send(&reply);
                    }
                    if let Some(m) = cache.lookup(dst_ip) { dst_mac = Some(m); break; }
                }
                unsafe { core::arch::asm!("pause"); }
            }
            let dst_mac = match dst_mac {
                Some(m) => m,
                None => { vga::term_puts("[tcp] no ARP reply\n", vga::C_RED); return; }
            };

            // ── SYN ──────────────────────────────────────────────────────
            let src_port: u16 = 49152 + ((math::rdtsc() as u16) & 0x0FFF);
            let our_isn: u32 = math::rdtsc() as u32;
            let mut frame = [0u8; 1500];
            let n = net::build_tcp(
                drv.nic.mac, our_ip, src_port, dst_mac, dst_ip, dst_port,
                our_isn, 0, net::TCP_SYN, 8192, &[], &mut frame
            ).unwrap_or(0);
            vga::term_puts("[tcp] SYN -> ", vga::C_CYAN);
            print_ipv4(dst_ip); vga::term_puts(":", vga::C_DIM); vga::term_putu32(dst_port as u32, vga::C_ORANGE);
            vga::term_puts("\n", 0);
            if drv.send(&frame[..n]).is_err() {
                vga::term_puts("[tcp] SYN TX failed\n", vga::C_RED); return;
            }

            // ── Wait for SYN-ACK ─────────────────────────────────────────
            let mut peer_seq: u32 = 0;
            let mut got_synack = false;
            for _ in 0..2_000_000u32 {
                if let Some((idx, _)) = drv.poll_rx() {
                    let f = drv.rx_frame(idx);
                    if let Some(seg) = net::parse_tcp(f) {
                        if seg.dst_port == src_port
                           && seg.flags & net::TCP_SYN != 0
                           && seg.flags & net::TCP_ACK != 0
                        {
                            peer_seq = seg.seq;
                            got_synack = true;
                            break;
                        }
                    }
                }
                unsafe { core::arch::asm!("pause"); }
            }
            if !got_synack { vga::term_puts("[tcp] no SYN-ACK\n", vga::C_RED); return; }
            vga::term_puts("[tcp] SYN-ACK received, peer_isn=", vga::C_GREEN);
            vga::term_putu32(peer_seq, vga::C_ORANGE);
            vga::term_puts("\n", 0);

            // ── ACK (3rd of handshake) ───────────────────────────────────
            let our_seq = our_isn.wrapping_add(1);
            let our_ack = peer_seq.wrapping_add(1);
            let n = net::build_tcp(
                drv.nic.mac, our_ip, src_port, dst_mac, dst_ip, dst_port,
                our_seq, our_ack, net::TCP_ACK, 8192, &[], &mut frame
            ).unwrap_or(0);
            let _ = drv.send(&frame[..n]);

            // ── Send the payload (PSH + ACK) ────────────────────────────
            let mut payload_seq = our_seq;
            if !text.is_empty() {
                let n = net::build_tcp(
                    drv.nic.mac, our_ip, src_port, dst_mac, dst_ip, dst_port,
                    payload_seq, our_ack, net::TCP_PSH | net::TCP_ACK, 8192,
                    text.as_bytes(), &mut frame
                ).unwrap_or(0);
                vga::term_puts("[tcp] TX ", vga::C_CYAN);
                vga::term_putu32(text.len() as u32, vga::C_ORANGE);
                vga::term_puts(" bytes\n", vga::C_CYAN);
                if drv.send(&frame[..n]).is_err() {
                    vga::term_puts("[tcp] payload TX failed\n", vga::C_RED);
                }
                payload_seq = payload_seq.wrapping_add(text.len() as u32);
            }

            // ── Read whatever comes back ────────────────────────────────
            vga::term_puts("[tcp] RX:\n", vga::C_CYAN);
            for _ in 0..2_000_000u32 {
                // Pull one frame and copy out everything we need so we can
                // free the immutable borrow before calling drv.send below.
                let action: Option<(u32, u8, alloc::vec::Vec<u8>)> = if let Some((idx, _)) = drv.poll_rx() {
                    let f = drv.rx_frame(idx);
                    if let Some(seg) = net::parse_tcp(f) {
                        if seg.dst_port == src_port {
                            let payload: alloc::vec::Vec<u8> = seg.payload.to_vec();
                            // Compute next-ack we'd send.
                            let len_consumed = payload.len() as u32
                                             + if seg.flags & net::TCP_FIN != 0 { 1 } else { 0 };
                            Some((seg.seq.wrapping_add(len_consumed), seg.flags, payload))
                        } else { None }
                    } else { None }
                } else { None };

                if let Some((peer_ack, flags, payload)) = action {
                    // Print printable payload bytes
                    for &b in &payload {
                        if (32..127).contains(&b) || b == b'\n' || b == b'\r' {
                            vga::term_putc(b, vga::C_TEXT);
                        }
                    }
                    // Always ACK
                    let tcp_flags = if flags & net::TCP_FIN != 0 {
                        net::TCP_FIN | net::TCP_ACK
                    } else {
                        net::TCP_ACK
                    };
                    let n = net::build_tcp(
                        drv.nic.mac, our_ip, src_port, dst_mac, dst_ip, dst_port,
                        payload_seq, peer_ack, tcp_flags, 8192, &[], &mut frame
                    ).unwrap_or(0);
                    let _ = drv.send(&frame[..n]);
                    if flags & net::TCP_FIN != 0 {
                        vga::term_puts("\n[tcp] FIN, connection closed\n", vga::C_DIM);
                        return;
                    }
                }
                unsafe { core::arch::asm!("pause"); }
            }
            vga::term_puts("\n[tcp] poll timeout (connection still open)\n", vga::C_DIM);
        }
        "/http" => {
            // Format: /http <host> [path]
            // Resolves host via DNS, opens TCP:80, GETs path, prints body.
            if arg.is_empty() {
                vga::term_puts("usage: /http <host> [path]\n", vga::C_RED); return;
            }
            let (host, path) = match arg.find(char::is_whitespace) {
                Some(i) => (&arg[..i], arg[i..].trim_start()),
                None => (arg, "/"),
            };
            let path = if path.is_empty() { "/" } else { path };

            vga::term_puts("[http] resolving ", vga::C_CYAN);
            vga::term_puts(host, vga::C_CYAN);
            vga::term_puts("...\n", vga::C_DIM);
            let target_ip = match resolve_host(host) {
                Ok(ip) => ip,
                Err(e) => {
                    vga::term_puts("[http] DNS error: ", vga::C_RED);
                    vga::term_puts(e, vga::C_RED);
                    vga::term_puts("\n", 0);
                    return;
                }
            };
            vga::term_puts("[http] ", vga::C_DIM);
            vga::term_puts(host, vga::C_CYAN);
            vga::term_puts(" -> ", vga::C_DIM);
            print_ipv4(target_ip);
            vga::term_puts("\n", 0);

            // Build the request inline using alloc::String to avoid format!.
            let mut req = alloc::string::String::with_capacity(host.len() + path.len() + 50);
            req.push_str("GET ");
            req.push_str(path);
            req.push_str(" HTTP/1.0\r\nHost: ");
            req.push_str(host);
            req.push_str("\r\nUser-Agent: MatrixOS\r\nConnection: close\r\n\r\n");

            // Reuse the same TCP path used by /tcp.  We open a new Driver
            // because resolve_host already brought one up; that's fine — the
            // NIC is fast to re-init.
            let hhdm = unsafe { HHDM_OFFSET };
            let our_ip = unsafe { OUR_IP };
            let gateway = unsafe { GATEWAY_IP };
            let nic = match net::probe(hhdm) {
                Some(n) => n,
                None => { vga::term_puts("no NIC\n", vga::C_RED); return; }
            };
            let mut drv = net::Driver::init(nic, hhdm);
            let mut cache = net::ArpCache::new();

            // ARP gateway (HTTP traffic usually leaves the subnet)
            drv.send_arp(our_ip, gateway).ok();
            let mut gw_mac: Option<[u8; 6]> = None;
            for _ in 0..400_000u32 {
                if let Some((idx, _)) = drv.poll_rx() {
                    let f = drv.rx_frame(idx);
                    if let Some(reply) = net::process_arp(f, &mut cache, drv.nic.mac, our_ip) {
                        let _ = drv.send(&reply);
                    }
                    if let Some(m) = cache.lookup(gateway) { gw_mac = Some(m); break; }
                }
                unsafe { core::arch::asm!("pause"); }
            }
            let gw_mac = match gw_mac {
                Some(m) => m,
                None => { vga::term_puts("[http] gateway ARP timeout\n", vga::C_RED); return; }
            };

            // TCP handshake
            let src_port: u16 = 49152 + ((math::rdtsc() as u16) & 0x0FFF);
            let our_isn: u32 = math::rdtsc() as u32;
            let mut frame = [0u8; 1500];
            let n = net::build_tcp(
                drv.nic.mac, our_ip, src_port, gw_mac, target_ip, 80,
                our_isn, 0, net::TCP_SYN, 8192, &[], &mut frame
            ).unwrap_or(0);
            vga::term_puts("[http] SYN ", vga::C_CYAN);
            print_ipv4(target_ip);
            vga::term_puts(":80\n", vga::C_DIM);
            if drv.send(&frame[..n]).is_err() {
                vga::term_puts("[http] SYN TX failed\n", vga::C_RED); return;
            }

            let mut peer_seq = 0u32;
            let mut got = false;
            for _ in 0..2_000_000u32 {
                if let Some((idx, _)) = drv.poll_rx() {
                    let f = drv.rx_frame(idx);
                    if let Some(seg) = net::parse_tcp(f) {
                        if seg.dst_port == src_port
                            && seg.flags & net::TCP_SYN != 0
                            && seg.flags & net::TCP_ACK != 0
                        {
                            peer_seq = seg.seq;
                            got = true;
                            break;
                        }
                    }
                }
                unsafe { core::arch::asm!("pause"); }
            }
            if !got { vga::term_puts("[http] SYN-ACK timeout\n", vga::C_RED); return; }

            let our_seq = our_isn.wrapping_add(1);
            let our_ack = peer_seq.wrapping_add(1);
            // ACK
            let n = net::build_tcp(
                drv.nic.mac, our_ip, src_port, gw_mac, target_ip, 80,
                our_seq, our_ack, net::TCP_ACK, 8192, &[], &mut frame
            ).unwrap_or(0);
            let _ = drv.send(&frame[..n]);

            // Send request
            let n = net::build_tcp(
                drv.nic.mac, our_ip, src_port, gw_mac, target_ip, 80,
                our_seq, our_ack, net::TCP_PSH | net::TCP_ACK, 8192,
                req.as_bytes(), &mut frame
            ).unwrap_or(0);
            vga::term_puts("[http] GET ", vga::C_DIM);
            vga::term_puts(path, vga::C_CYAN);
            vga::term_puts("\n[http] ── response ──\n", vga::C_DIM);
            let _ = drv.send(&frame[..n]);
            let mut payload_seq = our_seq.wrapping_add(req.len() as u32);

            // Read response
            let mut total_rx = 0u32;
            for _ in 0..5_000_000u32 {
                let action: Option<(u32, u8, alloc::vec::Vec<u8>)> = if let Some((idx, _)) = drv.poll_rx() {
                    let f = drv.rx_frame(idx);
                    if let Some(seg) = net::parse_tcp(f) {
                        if seg.dst_port == src_port {
                            let payload: alloc::vec::Vec<u8> = seg.payload.to_vec();
                            let len_consumed = payload.len() as u32
                                             + if seg.flags & net::TCP_FIN != 0 { 1 } else { 0 };
                            Some((seg.seq.wrapping_add(len_consumed), seg.flags, payload))
                        } else { None }
                    } else { None }
                } else { None };

                if let Some((peer_ack, flags, payload)) = action {
                    for &b in &payload {
                        if (32..127).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t' {
                            vga::term_putc(b, vga::C_TEXT);
                        }
                    }
                    total_rx += payload.len() as u32;
                    let tcp_flags = if flags & net::TCP_FIN != 0 {
                        net::TCP_FIN | net::TCP_ACK
                    } else { net::TCP_ACK };
                    let n = net::build_tcp(
                        drv.nic.mac, our_ip, src_port, gw_mac, target_ip, 80,
                        payload_seq, peer_ack, tcp_flags, 8192, &[], &mut frame
                    ).unwrap_or(0);
                    let _ = drv.send(&frame[..n]);
                    if flags & net::TCP_FIN != 0 {
                        vga::term_puts("\n[http] connection closed.  ", vga::C_DIM);
                        vga::term_putu32(total_rx, vga::C_ORANGE);
                        vga::term_puts(" bytes received\n", vga::C_DIM);
                        return;
                    }
                }
                unsafe { core::arch::asm!("pause"); }
            }
            vga::term_puts("\n[http] poll timeout\n", vga::C_DIM);
        }
        "/fs" => {
            // /fs ls
            // /fs format
            // /fs save <name> <text...>
            // /fs cat  <name>
            // /fs rm   <name>
            let (sub, sub_arg) = match arg.find(char::is_whitespace) {
                Some(i) => (&arg[..i], arg[i..].trim_start()),
                None    => (arg, ""),
            };
            match sub {
                "" | "ls" => {
                    match fs::list() {
                        Ok(files) => {
                            vga::term_puts("Files (", vga::C_CYAN);
                            vga::term_putu32(files.len() as u32, vga::C_ORANGE);
                            vga::term_puts(" / ", vga::C_DIM);
                            vga::term_putu32(fs::MAX_FILES as u32, vga::C_DIM);
                            vga::term_puts("):\n", vga::C_CYAN);
                            for (name, size) in &files {
                                vga::term_puts("  ", 0);
                                vga::term_puts(name, vga::C_GREEN);
                                vga::term_puts("   ", vga::C_DIM);
                                vga::term_putu32(*size, vga::C_ORANGE);
                                vga::term_puts(" bytes\n", vga::C_DIM);
                            }
                            if files.is_empty() {
                                vga::term_puts("  (empty — try /fs format first)\n", vga::C_DIM);
                            }
                        }
                        Err(e) => {
                            vga::term_puts("[fs] ls error: ", vga::C_RED);
                            vga::term_puts(e, vga::C_RED);
                            vga::term_puts("\n", 0);
                        }
                    }
                }
                "format" => {
                    match fs::format() {
                        Ok(()) => vga::term_puts("[fs] formatted (empty directory)\n", vga::C_GREEN),
                        Err(e) => { vga::term_puts("[fs] error: ", vga::C_RED); vga::term_puts(e, vga::C_RED); vga::term_puts("\n", 0); }
                    }
                }
                "save" => {
                    let (name, text) = match sub_arg.find(char::is_whitespace) {
                        Some(i) => (&sub_arg[..i], sub_arg[i..].trim_start()),
                        None    => (sub_arg, ""),
                    };
                    if name.is_empty() {
                        vga::term_puts("usage: /fs save <name> <text>\n", vga::C_RED); return;
                    }
                    match fs::save(name, text.as_bytes()) {
                        Ok(()) => {
                            vga::term_puts("[fs] saved ", vga::C_GREEN);
                            vga::term_puts(name, vga::C_GREEN);
                            vga::term_puts(" (", vga::C_DIM);
                            vga::term_putu32(text.len() as u32, vga::C_ORANGE);
                            vga::term_puts(" bytes)\n", vga::C_DIM);
                        }
                        Err(e) => { vga::term_puts("[fs] error: ", vga::C_RED); vga::term_puts(e, vga::C_RED); vga::term_puts("\n", 0); }
                    }
                }
                "cat" => {
                    if sub_arg.is_empty() { vga::term_puts("usage: /fs cat <name>\n", vga::C_RED); return; }
                    match fs::load(sub_arg) {
                        Ok(data) => {
                            vga::term_puts("--- ", vga::C_DIM);
                            vga::term_puts(sub_arg, vga::C_GREEN);
                            vga::term_puts(" (", vga::C_DIM);
                            vga::term_putu32(data.len() as u32, vga::C_ORANGE);
                            vga::term_puts(" bytes) ---\n", vga::C_DIM);
                            for &b in &data {
                                if (32..127).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t' {
                                    vga::term_putc(b, vga::C_TEXT);
                                }
                            }
                            vga::term_puts("\n--- end ---\n", vga::C_DIM);
                        }
                        Err(e) => { vga::term_puts("[fs] error: ", vga::C_RED); vga::term_puts(e, vga::C_RED); vga::term_puts("\n", 0); }
                    }
                }
                "rm" => {
                    if sub_arg.is_empty() { vga::term_puts("usage: /fs rm <name>\n", vga::C_RED); return; }
                    match fs::delete(sub_arg) {
                        Ok(()) => { vga::term_puts("[fs] removed ", vga::C_GREEN); vga::term_puts(sub_arg, vga::C_GREEN); vga::term_puts("\n", 0); }
                        Err(e) => { vga::term_puts("[fs] error: ", vga::C_RED); vga::term_puts(e, vga::C_RED); vga::term_puts("\n", 0); }
                    }
                }
                _ => {
                    vga::term_puts("unknown subcommand. try: /fs ls | save | cat | rm | format\n", vga::C_RED);
                }
            }
        }
        "/resolve" => {
            if arg.is_empty() {
                vga::term_puts("usage: /resolve <hostname>\n", vga::C_RED); return;
            }
            vga::term_puts("[dns] resolving ", vga::C_CYAN);
            vga::term_puts(arg, vga::C_CYAN);
            vga::term_puts(" via ", vga::C_DIM);
            print_ipv4(unsafe { DNS_IP });
            vga::term_puts("\n", 0);
            match resolve_host(arg) {
                Ok(ip) => {
                    vga::term_puts("  -> ", vga::C_GREEN);
                    print_ipv4(ip);
                    vga::term_puts("\n", 0);
                }
                Err(e) => {
                    vga::term_puts("  error: ", vga::C_RED);
                    vga::term_puts(e, vga::C_RED);
                    vga::term_puts("\n", 0);
                }
            }
        }
        "/dhcp" => {
            // Full 4-way DHCP exchange to auto-configure our IP, gateway,
            // netmask, DNS.  Broadcast media so no ARP needed up front.
            let hhdm = unsafe { HHDM_OFFSET };
            let nic = match net::probe(hhdm) {
                Some(n) => n,
                None => { vga::term_puts("no NIC\n", vga::C_RED); return; }
            };
            let mut drv = net::Driver::init(nic, hhdm);
            let xid: u32 = (math::rdtsc() as u32) ^ 0xDEAD_BEEF;
            let bcast_mac = [0xFFu8; 6];
            let bcast_ip  = [0xFFu8; 4];

            // ── 1. DHCPDISCOVER ──────────────────────────────────────────
            vga::term_puts("[dhcp] DISCOVER...\n", vga::C_CYAN);
            let mut dhcp = [0u8; 300];
            net::build_dhcp(drv.nic.mac, xid, 1, None, None, &mut dhcp);
            let mut frame = [0u8; 400];
            let n = match net::build_udp(
                drv.nic.mac, [0,0,0,0], net::DHCP_CLIENT_PORT,
                bcast_mac, bcast_ip, net::DHCP_SERVER_PORT,
                &dhcp, &mut frame,
            ) { Some(n) => n, None => { vga::term_puts("build_udp failed\n", vga::C_RED); return; }};
            if drv.send(&frame[..n]).is_err() {
                vga::term_puts("[dhcp] DISCOVER TX failed\n", vga::C_RED); return;
            }

            // ── 2. Wait for DHCPOFFER ────────────────────────────────────
            let mut offer: Option<net::DhcpLease> = None;
            for _ in 0..2_000_000u32 {
                if let Some((idx, _len)) = drv.poll_rx() {
                    let f = drv.rx_frame(idx);
                    if let Some(pkt) = net::parse_udp(f) {
                        if pkt.dst_port == net::DHCP_CLIENT_PORT {
                            if let Some((mt, lease)) = net::parse_dhcp(pkt.payload) {
                                if mt == 2 { offer = Some(lease); break; }
                            }
                        }
                    }
                }
                unsafe { core::arch::asm!("pause"); }
            }
            let lease = match offer {
                Some(l) => l,
                None => { vga::term_puts("[dhcp] no OFFER received\n", vga::C_RED); return; }
            };
            vga::term_puts("[dhcp] OFFER ", vga::C_GREEN);
            print_ipv4(lease.yiaddr);
            vga::term_puts("  server ", vga::C_DIM);
            print_ipv4(lease.server);
            vga::term_puts("\n", 0);

            // ── 3. DHCPREQUEST ──────────────────────────────────────────
            vga::term_puts("[dhcp] REQUEST...\n", vga::C_CYAN);
            net::build_dhcp(drv.nic.mac, xid, 3, Some(lease.yiaddr), Some(lease.server), &mut dhcp);
            let n = net::build_udp(
                drv.nic.mac, [0,0,0,0], net::DHCP_CLIENT_PORT,
                bcast_mac, bcast_ip, net::DHCP_SERVER_PORT,
                &dhcp, &mut frame,
            ).unwrap_or(0);
            if drv.send(&frame[..n]).is_err() {
                vga::term_puts("[dhcp] REQUEST TX failed\n", vga::C_RED); return;
            }

            // ── 4. Wait for DHCPACK ──────────────────────────────────────
            let mut ack: Option<net::DhcpLease> = None;
            for _ in 0..2_000_000u32 {
                if let Some((idx, _len)) = drv.poll_rx() {
                    let f = drv.rx_frame(idx);
                    if let Some(pkt) = net::parse_udp(f) {
                        if pkt.dst_port == net::DHCP_CLIENT_PORT {
                            if let Some((mt, lease)) = net::parse_dhcp(pkt.payload) {
                                if mt == 5 { ack = Some(lease); break; }
                            }
                        }
                    }
                }
                unsafe { core::arch::asm!("pause"); }
            }
            let lease = match ack {
                Some(l) => l,
                None => { vga::term_puts("[dhcp] no ACK received\n", vga::C_RED); return; }
            };

            // ── Apply ───────────────────────────────────────────────────
            unsafe {
                OUR_IP     = lease.yiaddr;
                GATEWAY_IP = lease.gateway;
                if lease.dns != [0; 4] { DNS_IP = lease.dns; }
            }
            vga::term_puts("[dhcp] ACK  ip=", vga::C_GREEN);
            print_ipv4(lease.yiaddr);
            vga::term_puts(" mask=", vga::C_DIM); print_ipv4(lease.netmask);
            vga::term_puts(" gw=",   vga::C_DIM); print_ipv4(lease.gateway);
            vga::term_puts(" dns=",  vga::C_DIM); print_ipv4(lease.dns);
            vga::term_puts("\n[dhcp] OUR_IP configured.\n", vga::C_GREEN);
        }
        "/ping" => {
            // Full ARP-then-ICMP-echo cycle.  Assumes the target is on the
            // same subnet (no routing yet).
            let hhdm = unsafe { HHDM_OFFSET };
            let target_ip = match parse_ipv4(arg) {
                Some(ip) => ip,
                None => { vga::term_puts("usage: /ping <a.b.c.d>\n", vga::C_RED); return; }
            };
            let nic = match net::probe(hhdm) {
                Some(n) => n,
                None => { vga::term_puts("no NIC\n", vga::C_RED); return; }
            };
            let our_ip = unsafe { OUR_IP };

            vga::term_puts("PING ", vga::C_CYAN);
            for (i, &b) in target_ip.iter().enumerate() {
                if i > 0 { vga::term_putc(b'.', vga::C_DIM); }
                vga::term_putu32(b as u32, vga::C_ORANGE);
            }
            vga::term_puts("  from ", vga::C_DIM);
            for (i, &b) in our_ip.iter().enumerate() {
                if i > 0 { vga::term_putc(b'.', vga::C_DIM); }
                vga::term_putu32(b as u32, vga::C_CYAN);
            }
            vga::term_puts("\n", 0);

            let mut drv = net::Driver::init(nic, hhdm);
            let mut cache = net::ArpCache::new();

            // Step 1: ARP for the target
            vga::term_puts("  arp who-has...\n", vga::C_DIM);
            if drv.send_arp(our_ip, target_ip).is_err() {
                vga::term_puts("  ARP TX failed\n", vga::C_RED); return;
            }
            let mut target_mac: Option<[u8; 6]> = None;
            for _ in 0..400_000u32 {
                if let Some((idx, _len)) = drv.poll_rx() {
                    let frame = drv.rx_frame(idx);
                    // Process ARP into cache + auto-reply if asked for us
                    if let Some(reply) = net::process_arp(frame, &mut cache,
                                                          drv.nic.mac, our_ip)
                    {
                        // Send the reply (best-effort, ignore errors)
                        let _ = drv.send(&reply);
                    }
                    if let Some(mac) = cache.lookup(target_ip) {
                        target_mac = Some(mac);
                        break;
                    }
                }
                unsafe { core::arch::asm!("pause"); }
            }
            let dst_mac = match target_mac {
                Some(m) => m,
                None => {
                    vga::term_puts("  no ARP reply (VBox NAT may not respond — use Bridged)\n", vga::C_RED);
                    return;
                }
            };
            vga::term_puts("  target MAC: ", vga::C_DIM);
            for (i, &b) in dst_mac.iter().enumerate() {
                if i > 0 { vga::term_putc(b':', vga::C_DIM); }
                vga::term_puthex8(b, vga::C_CYAN);
            }
            vga::term_puts("\n", 0);

            // Step 2: ICMP echo
            let mut frame = [0u8; 64];
            let payload = b"MatrixOS";
            let n = match net::build_icmp_echo(
                drv.nic.mac, our_ip, dst_mac, target_ip,
                0xBEEF, 1, payload, &mut frame
            ) {
                Some(n) => n,
                None => { vga::term_puts("  build_icmp_echo failed\n", vga::C_RED); return; }
            };
            vga::term_puts("  ICMP echo TX (", vga::C_CYAN);
            vga::term_putu32(n as u32, vga::C_ORANGE);
            vga::term_puts(" bytes)...\n", vga::C_CYAN);
            if drv.send(&frame[..n]).is_err() {
                vga::term_puts("  ICMP TX failed\n", vga::C_RED); return;
            }

            // Step 3: wait for echo-reply
            for _ in 0..400_000u32 {
                if let Some((idx, _len)) = drv.poll_rx() {
                    let f = drv.rx_frame(idx);
                    if let Some((ident, seq, plen)) = net::parse_icmp_reply(f, our_ip) {
                        vga::term_puts("  REPLY: ident=0x", vga::C_GREEN);
                        vga::term_puthex8((ident >> 8) as u8, vga::C_ORANGE);
                        vga::term_puthex8( ident       as u8, vga::C_ORANGE);
                        vga::term_puts(" seq=", vga::C_DIM);
                        vga::term_putu32(seq as u32, vga::C_ORANGE);
                        vga::term_puts(" payload=", vga::C_DIM);
                        vga::term_putu32(plen as u32, vga::C_ORANGE);
                        vga::term_puts(" bytes  PING OK\n", vga::C_GREEN);
                        return;
                    }
                }
                unsafe { core::arch::asm!("pause"); }
            }
            vga::term_puts("  no ICMP reply received\n", vga::C_RED);
        }
        "/beep" => {
            let (freq_s, ms_s) = match arg.find(char::is_whitespace) {
                Some(i) => (&arg[..i], arg[i..].trim_start()),
                None    => (arg, ""),
            };
            let freq = parse_u32_safe(freq_s).unwrap_or(880);
            let ms   = parse_u32_safe(ms_s).unwrap_or(150);
            audio::beep_blocking(freq, ms);
            vga::term_puts("[beep] ", vga::C_DIM);
            vga::term_putu32(freq, vga::C_ORANGE);
            vga::term_puts(" Hz / ", vga::C_DIM);
            vga::term_putu32(ms, vga::C_ORANGE);
            vga::term_puts(" ms\n", vga::C_DIM);
        }
        "/rand" => {
            let n = rng::rand_u64();
            vga::term_puts("rand = 0x", vga::C_DIM);
            // 16 hex digits
            for i in (0..16).rev() {
                let d = ((n >> (i * 4)) & 0xF) as u8;
                let c = if d < 10 { b'0' + d } else { b'A' + (d - 10) };
                vga::term_putc(c, vga::C_ORANGE);
            }
            vga::term_puts(if rng::has_hardware() {"  (RDRAND)\n"} else {"  (PRNG)\n"}, vga::C_DIM);
        }
        "/meminfo" => {
            let used = allocator::heap_used();
            let free = allocator::heap_free();
            let total = used + free;
            vga::term_puts("Heap (bytes):\n", vga::C_CYAN);
            vga::term_puts("  used: ", vga::C_DIM);
            vga::term_putu32((used / 1024) as u32, vga::C_ORANGE);
            vga::term_puts(" KB\n", vga::C_DIM);
            vga::term_puts("  free: ", vga::C_DIM);
            vga::term_putu32((free / (1024*1024)) as u32, vga::C_ORANGE);
            vga::term_puts(" MB\n", vga::C_DIM);
            vga::term_puts("  total:", vga::C_DIM);
            vga::term_putu32((total / (1024*1024)) as u32, vga::C_ORANGE);
            vga::term_puts(" MB", vga::C_DIM);
            vga::term_puts("  (utilisation: ", vga::C_DIM);
            let pct = if total > 0 { (used * 100 / total) as u32 } else { 0 };
            vga::term_putu32(pct, vga::C_ORANGE);
            vga::term_puts("%)\n", vga::C_DIM);
        }
        "/uptime" => {
            // Prefer the PIT-driven millisecond counter (cheap, monotonic).
            // Fall back to TSC if the PIT hasn't ticked yet.
            let ms_pit = irq::tick_ms();
            let (secs, ms) = if ms_pit > 0 {
                (ms_pit / 1000, ms_pit % 1000)
            } else {
                let elapsed_ticks = tsc.wrapping_sub(boot_tsc);
                let hz = math::tsc_hz();
                (elapsed_ticks / hz, (elapsed_ticks % hz) * 1000 / hz)
            };
            vga::term_puts("Uptime: ", vga::C_DIM);
            vga::term_putu32(secs as u32, vga::C_GREEN);
            vga::term_puts(".", vga::C_DIM);
            if ms < 100 { vga::term_putc(b'0', vga::C_GREEN); }
            if ms <  10 { vga::term_putc(b'0', vga::C_GREEN); }
            vga::term_putu32(ms as u32, vga::C_GREEN);
            vga::term_puts(" s  (PIT @100 Hz)\n", vga::C_DIM);
        }
        "/delete" => {
            if arg.is_empty() {
                vga::term_puts("usage: /delete <app-name>\n", vga::C_RED);
                return;
            }
            match registry.delete(arg) {
                Ok(()) => {
                    vga::term_puts("[delete] removed: ", vga::C_GREEN);
                    vga::term_puts(arg, vga::C_GREEN);
                    vga::term_puts("\n", 0);
                }
                Err(e) => {
                    vga::term_puts("[delete] error: ", vga::C_RED);
                    vga::term_puts(e, vga::C_RED);
                    vga::term_puts("\n", 0);
                }
            }
        }
        "/show" => {
            if arg.is_empty() {
                vga::term_puts("usage: /show <app-name>\n", vga::C_RED);
                return;
            }
            match registry.find(arg) {
                Some(app) => {
                    vga::term_puts("--- ", vga::C_DIM);
                    vga::term_puts(&app.name, vga::C_GREEN);
                    vga::term_puts(" (", vga::C_DIM);
                    vga::term_puts(app.author.label(), vga::C_DIM);
                    vga::term_puts(") ---\n", vga::C_DIM);
                    for &b in app.text.as_bytes() {
                        vga::term_putc(b, vga::C_TEXT);
                    }
                    vga::term_puts("\n--- end ---\n", vga::C_DIM);
                }
                None => {
                    vga::term_puts("no such app: ", vga::C_RED);
                    vga::term_puts(arg, vga::C_RED);
                    vga::term_puts("\n", 0);
                }
            }
        }
        "/save" => {
            let settings = state::Settings {
                max_chat:     *max_new_chat as u32,
                max_code:     *max_new_code as u32,
                auto_pick:    *auto_pick,
                total_tokens,
            };
            match state::save(settings, registry) {
                Ok(()) => vga::term_puts("[save] OK — state persisted to disk LBA 8\n", vga::C_GREEN),
                Err(e) => {
                    vga::term_puts("[save] error: ", vga::C_RED);
                    vga::term_puts(e, vga::C_RED);
                    vga::term_puts("\n", 0);
                }
            }
        }
        "/load" => {
            match state::load(registry) {
                Some(s) => {
                    *max_new_chat = s.max_chat.clamp(32, 512) as usize;
                    *max_new_code = s.max_code.clamp(32, 640) as usize;
                    *auto_pick    = s.auto_pick;
                    vga::term_puts("[load] OK — settings restored, ", vga::C_GREEN);
                    let total = registry.list().len() as u32;
                    vga::term_putu32(total, vga::C_ORANGE);
                    vga::term_puts(" apps total\n", vga::C_GREEN);
                }
                None => vga::term_puts("[load] no saved state on disk (or magic mismatch)\n", vga::C_DIM),
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

// ── /install: real disk image write ──────────────────────────────────────────
//
// Layout we lay down on the target disk:
//   LBA 0           "MATRIXOS" MBR header (no real bootloader yet, see notes)
//   LBA 1           kernel ELF (small — under 1 MB in practice)
//   LBA 4097..       model bundle (3 GB GGUF stream, what we received as
//                    the Limine module)
//
// The boot record at LBA 0 isn't directly bootable by BIOS yet — that
// requires 16-bit real-mode loader code we don't have here.  But the data
// layout is documented and verifiable, and a future limine-or-bootsect
// fix can read from this image instead of the CD.

// ── Planner pipeline: User → LLM → JSON → Executor ─────────────────────────

const PLAN_SYSTEM: &str = "\
You are the MatrixOS planner.  Given the user's request in natural language,\
emit ONLY a JSON array of action objects — no prose, no markdown fences.\n\
Each object: {\"action\":\"VERB\", ...key:value pairs}.\n\
Available verbs:\n\
  say     text=\"...\"                              print text\n\
  beep    freq=NNNN  ms=NNN                       (defaults 880, 150)\n\
  wait    ms=NNN                                   sleep\n\
  clear                                            clear screen\n\
  set     key=max_chat|max_code|auto  value=...    change a setting\n\
  run     app=NAME                                 run an installed app\n\
  fs_save name=...   body=\"...\"                    write a file\n\
  fs_load name=...                                  print file\n\
  fs_list                                           list files\n\
  fs_rm   name=...                                  delete file\n\
  rand                                              print a random u64\n\
Keep plans short (≤ 6 steps).  Output JSON only.";

const PLAN_FIX_SYSTEM: &str = "\
You are a strict JSON-validator for the MatrixOS planner DSL.\
The user gives you a broken plan plus a parse error.  Output ONLY the\
corrected plan as a JSON array.  No prose.";

fn do_plan(
    user_request: &str,
    chat_gguf: &gguf::Gguf, chat_cfg: &model::ModelConfig,
    chat_kv: &mut model::KVCache, chat_tok: &Option<tokenizer::Tokenizer>,
    code_gguf: &gguf::Gguf, code_cfg: &model::ModelConfig,
    code_kv: &mut model::KVCache, code_tok: &Option<tokenizer::Tokenizer>,
    scratch: &mut model::ForwardScratch,
    registry: &mut apps::Registry,
    max_new_chat: &mut usize,
    max_new_code: &mut usize,
    auto_pick: &mut bool,
) {
    let req = user_request.trim();
    if req.is_empty() {
        vga::term_puts("usage: /plan <natural-language request>\n", vga::C_RED);
        return;
    }

    vga::term_puts("[plan] request: ", vga::C_DIM);
    vga::term_puts(req, vga::C_TEXT);
    vga::term_puts("\n[plan] chat-AI generating plan...\n", vga::C_CYAN);

    // Round 1: chat-AI emits JSON plan.
    let mut plan_text = ai_turn_capture(
        chat_gguf, chat_cfg, chat_kv, scratch, chat_tok,
        PLAN_SYSTEM, req, 256,
    );

    const MAX_ROUNDS: usize = 3;
    for round in 1..=MAX_ROUNDS {
        match planner::parse_plan(&plan_text) {
            Ok(actions) if !actions.is_empty() => {
                vga::term_puts("[plan] parsed ", vga::C_GREEN);
                vga::term_putu32(actions.len() as u32, vga::C_ORANGE);
                vga::term_puts(" actions, executing:\n", vga::C_GREEN);
                let results = executor::run_plan(
                    &actions, registry, max_new_chat, max_new_code, auto_pick,
                );
                let ok_count  = results.iter().filter(|r| r.ok).count();
                let fail_count = results.iter().filter(|r| !r.ok).count();
                vga::term_puts("[plan] done: ", vga::C_DIM);
                vga::term_putu32(ok_count as u32, vga::C_GREEN);
                vga::term_puts(" OK, ", vga::C_DIM);
                vga::term_putu32(fail_count as u32, if fail_count > 0 { vga::C_RED } else { vga::C_DIM });
                vga::term_puts(" failed\n", vga::C_DIM);
                return;
            }
            Ok(_) => {
                vga::term_puts("[plan] empty plan, asking code-AI to fix\n", vga::C_ORANGE);
            }
            Err(e) => {
                vga::term_puts("[plan] parse error @ ", vga::C_RED);
                vga::term_putu32(e.byte_offset as u32, vga::C_RED);
                vga::term_puts(": ", vga::C_RED);
                vga::term_puts(e.message, vga::C_RED);
                vga::term_puts("\n", 0);
            }
        }
        if round == MAX_ROUNDS { break; }

        // Code-AI fix attempt
        vga::term_puts("[plan] round ", vga::C_CYAN);
        vga::term_putu32((round + 1) as u32, vga::C_CYAN);
        vga::term_puts(" code-AI fixing...\n", vga::C_CYAN);
        let mut fix_prompt = alloc::string::String::from("Broken plan:\n");
        fix_prompt.push_str(&plan_text);
        fix_prompt.push_str("\n\nRewrite as valid JSON.");
        plan_text = ai_turn_capture(
            code_gguf, code_cfg, code_kv, scratch, code_tok,
            PLAN_FIX_SYSTEM, &fix_prompt, 320,
        );
    }
    vga::term_puts("[plan] gave up after 3 rounds — try rephrasing\n", vga::C_RED);
}

/// Where to install — either an IDE drive slot (0..3) or an AHCI port.
enum InstallTarget {
    Ide(ata::DriveAddr),
    Ahci(u8 /* port */),
}

/// Read 1 sector from either target.
fn target_read(t: &InstallTarget, lba: u64, buf: &mut [u8]) -> Result<(), &'static str> {
    match t {
        InstallTarget::Ide(addr) => ata::read_sectors(*addr, lba as u32, 1, buf),
        InstallTarget::Ahci(port) => unsafe {
            ahci::read_sectors(HHDM_OFFSET, *port, lba, 1, buf)
        },
    }
}

/// Write 1 sector (used for MBR + verify).
fn target_write(t: &InstallTarget, lba: u64, buf: &[u8]) -> Result<(), &'static str> {
    match t {
        InstallTarget::Ide(addr) => ata::write_sectors(*addr, lba as u32, 1, buf),
        InstallTarget::Ahci(port) => unsafe {
            ahci::write_sectors(HHDM_OFFSET, *port, lba, 1, buf)
        },
    }
}

/// Stream the bundle.  Chunked at 32 sectors/call (AHCI PRDT entry limit
/// in our driver; IDE uses 64 in its own `install_image`).
fn target_stream(t: &InstallTarget, lba_start: u64, data: &[u8],
                 mut progress: impl FnMut(usize, usize)) -> Result<u32, &'static str> {
    match t {
        InstallTarget::Ide(addr) => {
            ata::install_image(*addr, lba_start as u32, data, |w, tot| progress(w, tot))
        }
        InstallTarget::Ahci(port) => {
            const SECTORS_PER_CALL: usize = 32;
            const BYTES_PER_CALL:   usize = SECTORS_PER_CALL * 512;
            let total = data.len();
            let mut written = 0usize;
            let mut lba = lba_start;
            let mut tmp = [0u8; BYTES_PER_CALL];
            while written < total {
                let n = (total - written).min(BYTES_PER_CALL);
                tmp[..n].copy_from_slice(&data[written..written + n]);
                if n < BYTES_PER_CALL {
                    for b in &mut tmp[n..] { *b = 0; }
                }
                let sectors = ((n + 511) / 512) as u16;
                unsafe {
                    ahci::write_sectors(HHDM_OFFSET, *port, lba, sectors, &tmp[..sectors as usize * 512])?;
                }
                lba     += sectors as u64;
                written += n;
                progress(written, total);
            }
            Ok((lba - lba_start) as u32)
        }
    }
}

fn do_install(arg: &str, mod_data: &[u8]) {
    let arg = arg.trim();
    if arg.is_empty() {
        vga::term_puts("usage: /install <slot>           dry-run (IDE slot)\n", vga::C_RED);
        vga::term_puts("       /install <slot> confirm   actually write\n", vga::C_RED);
        vga::term_puts("       /install ahci <port> [confirm]  install on SATA\n", vga::C_RED);
        return;
    }

    // Parse: support "ahci <port> [confirm]" or "<slot> [confirm]"
    let (head, rest) = match arg.find(char::is_whitespace) {
        Some(i) => (&arg[..i], arg[i..].trim_start()),
        None    => (arg, ""),
    };
    let (target, confirm_str): (InstallTarget, &str) = if head == "ahci" {
        let (port_s, rest2) = match rest.find(char::is_whitespace) {
            Some(i) => (&rest[..i], rest[i..].trim_start()),
            None    => (rest, ""),
        };
        let port = match parse_u32_safe(port_s) {
            Some(n) if n < 32 => n as u8,
            _ => { vga::term_puts("bad AHCI port (0..31)\n", vga::C_RED); return; }
        };
        (InstallTarget::Ahci(port), rest2)
    } else {
        let slot = match parse_u32_safe(head) {
            Some(n) if (n as usize) < 4 => n as usize,
            _ => { vga::term_puts("bad IDE slot (0..3)\n", vga::C_RED); return; }
        };
        let probe = ata::probe_all();
        let info = match probe.drives[slot].as_ref() {
            Some(d) => d,
            None    => { vga::term_puts("IDE slot empty — run /disks\n", vga::C_RED); return; }
        };
        if info.is_atapi {
            vga::term_puts("target is ATAPI (CD/DVD), refusing.\n", vga::C_RED);
            return;
        }
        (InstallTarget::Ide(info.addr), rest)
    };
    let confirm = confirm_str.trim() == "confirm";

    // Capacity / banner depending on target kind.
    let needed_bytes = mod_data.len() + 4096 * 512;
    match &target {
        InstallTarget::Ide(addr) => {
            vga::term_puts("[install] target: IDE ", vga::C_DIM);
            vga::term_puts(addr.label(), vga::C_GREEN);
            vga::term_puts("\n", 0);
        }
        InstallTarget::Ahci(port) => {
            vga::term_puts("[install] target: AHCI port ", vga::C_DIM);
            vga::term_putu32(*port as u32, vga::C_GREEN);
            vga::term_puts("\n", 0);
        }
    }
    vga::term_puts("[install] need:   ", vga::C_DIM);
    vga::term_putu32((needed_bytes / (1024 * 1024)) as u32, vga::C_ORANGE);
    vga::term_puts(" MB (kernel + bundle)\n", vga::C_DIM);

    // Read sector 0 to confirm read path works.
    let mut mbr = [0u8; 512];
    if let Err(e) = target_read(&target, 0, &mut mbr) {
        vga::term_puts("[install] read error: ", vga::C_RED);
        vga::term_puts(e, vga::C_RED);
        vga::term_puts("\n", 0);
        return;
    }
    vga::term_puts("[install] MBR sig before: 0x", vga::C_DIM);
    vga::term_puthex8(mbr[510], vga::C_ORANGE);
    vga::term_puthex8(mbr[511], vga::C_ORANGE);
    vga::term_puts("\n", 0);

    if !confirm {
        vga::term_puts("[install] dry-run only. Re-run with `confirm` to write.\n", vga::C_ORANGE);
        return;
    }

    // ── Step 1: bootable MBR ─────────────────────────────────────────────
    vga::term_puts("[install] writing bootable MBR (stage1)... ", vga::C_CYAN);
    let mut hdr = bootstub::MBR_STUB;
    // We can no longer overlay metadata at byte 440 — stage1 uses byte 0x180
    // for its drive byte AND the partition table area is now compiled code.
    // Put metadata RIGHT BEFORE the boot signature: bytes 480..510 (30 free).
    hdr[480..484].copy_from_slice(b"MTOS");
    hdr[484..488].copy_from_slice(&(mod_data.len() as u32).to_le_bytes());
    hdr[488..492].copy_from_slice(&(4097u32 + bootstub::STAGE2_SECTORS as u32).to_le_bytes());
    // bundle now starts AFTER stage2 sectors
    hdr[492..496].copy_from_slice(&((bootstub::STAGE2_SECTORS as u32) + 1).to_le_bytes());
    match target_write(&target, 0, &hdr) {
        Ok(()) => vga::term_puts("OK\n", vga::C_GREEN),
        Err(e) => {
            vga::term_puts("FAIL: ", vga::C_RED);
            vga::term_puts(e, vga::C_RED);
            vga::term_puts("\n", 0);
            return;
        }
    }

    // ── Step 1b: write stage2 (the 32-bit protected-mode bring-up) ─────
    vga::term_puts("[install] writing stage2 (", vga::C_CYAN);
    vga::term_putu32(bootstub::STAGE2_SECTORS as u32, vga::C_ORANGE);
    vga::term_puts(" sectors)... ", vga::C_CYAN);
    let stage2 = bootstub::STAGE2;
    let mut all_ok = true;
    for sector in 0..(bootstub::STAGE2_SECTORS as usize) {
        let lba = 1u64 + sector as u64;
        let off = sector * 512;
        if let Err(e) = target_write(&target, lba, &stage2[off..off + 512]) {
            vga::term_puts("FAIL: ", vga::C_RED);
            vga::term_puts(e, vga::C_RED);
            vga::term_puts("\n", 0);
            all_ok = false;
            break;
        }
    }
    if all_ok { vga::term_puts("OK\n", vga::C_GREEN); } else { return; }

    // ── Step 2: stream bundle ────────────────────────────────────────────
    vga::term_puts("[install] writing bundle (", vga::C_CYAN);
    vga::term_putu32((mod_data.len() / (1024 * 1024)) as u32, vga::C_ORANGE);
    vga::term_puts(" MB) — do not power off\n", vga::C_CYAN);
    let mut last_pct: u32 = 0;
    match target_stream(&target, 4097, mod_data, |w, t| {
        let pct = ((w as u64) * 100 / (t.max(1) as u64)) as u32;
        if pct != last_pct {
            vga::update_status_tokens(pct, 0);
            last_pct = pct;
        }
    }) {
        Ok(sectors) => {
            vga::term_puts("done — wrote ", vga::C_GREEN);
            vga::term_putu32(sectors, vga::C_GREEN);
            vga::term_puts(" sectors\n", vga::C_GREEN);
        }
        Err(e) => {
            vga::term_puts("FAIL: ", vga::C_RED);
            vga::term_puts(e, vga::C_RED);
            vga::term_puts("\n", 0);
            return;
        }
    }

    // ── Step 3: verify ───────────────────────────────────────────────────
    let mut verify = [0u8; 512];
    if target_read(&target, 0, &mut verify).is_ok()
        && &verify[480..484] == b"MTOS"
        && verify[510] == 0x55 && verify[511] == 0xAA
    {
        vga::term_puts("[install] verified stage1 + boot signature.\n", vga::C_GREEN);
        vga::term_puts("[install] INSTALL COMPLETE — disk boots into stage2:\n", vga::C_GREEN);
        vga::term_puts("[install]   stage1 (LBA 0) -> stage2 (LBA 1..)\n", vga::C_DIM);
        vga::term_puts("[install]   stage2: A20 + GDT + 32-bit PM\n", vga::C_DIM);
        vga::term_puts("[install]   Display: VGA text 0xB8000 'MATRIXOS BOOT OK'\n", vga::C_DIM);
        vga::term_puts("[install] (Long-mode + kernel-from-disk are the next PR.)\n", vga::C_DIM);
    } else {
        vga::term_puts("[install] WARNING: verify failed\n", vga::C_RED);
    }
}

// ── AI tool-call executor ────────────────────────────────────────────────────
//
// The chat AI may embed actions in its reply like:
//   <<TOOL: set max_chat=320>>
//   <<TOOL: run hello>>
//   <<TOOL: clear>>
// After streaming, we scan the response, parse the tool calls, and execute.
// Returns the number of tool calls fired.

fn execute_tool_calls(
    response: &str,
    registry: &mut apps::Registry,
    max_new_chat: &mut usize,
    max_new_code: &mut usize,
    auto_pick:    &mut bool,
) -> u32 {
    let bytes = response.as_bytes();
    let mut i = 0usize;
    let mut count = 0u32;
    while i + 8 <= bytes.len() {
        if &bytes[i..i+8] == b"<<TOOL: " {
            // Find the matching ">>"
            let start = i + 8;
            let mut end = start;
            while end + 1 < bytes.len() && !(bytes[end] == b'>' && bytes[end+1] == b'>') {
                end += 1;
            }
            if end + 1 < bytes.len() {
                let inner = core::str::from_utf8(&bytes[start..end]).unwrap_or("");
                run_one_tool(inner.trim(), registry, max_new_chat, max_new_code, auto_pick);
                count += 1;
                i = end + 2;
                continue;
            } else {
                break;
            }
        }
        i += 1;
    }
    count
}

fn run_one_tool(
    inner: &str,
    registry: &mut apps::Registry,
    max_new_chat: &mut usize,
    max_new_code: &mut usize,
    auto_pick:    &mut bool,
) {
    let (verb, arg) = match inner.find(char::is_whitespace) {
        Some(i) => (&inner[..i], inner[i..].trim_start()),
        None    => (inner, ""),
    };
    vga::term_puts("\n[tool] ", vga::C_DIM);
    match verb {
        "set" => {
            // Format: set key=value
            if let Some(eq) = arg.find('=') {
                let key = arg[..eq].trim();
                let val = arg[eq+1..].trim();
                match key {
                    "max_chat" => {
                        if let Some(n) = parse_u32_safe(val) {
                            *max_new_chat = (n as usize).clamp(32, 512);
                            vga::term_puts("set max_chat=", vga::C_GREEN);
                            vga::term_putu32(*max_new_chat as u32, vga::C_ORANGE);
                        }
                    }
                    "max_code" => {
                        if let Some(n) = parse_u32_safe(val) {
                            *max_new_code = (n as usize).clamp(32, 640);
                            vga::term_puts("set max_code=", vga::C_GREEN);
                            vga::term_putu32(*max_new_code as u32, vga::C_ORANGE);
                        }
                    }
                    "auto" => {
                        *auto_pick = matches!(val, "on" | "ON" | "true" | "1");
                        vga::term_puts("set auto=", vga::C_GREEN);
                        vga::term_puts(if *auto_pick {"on"} else {"off"}, vga::C_GREEN);
                    }
                    _ => { vga::term_puts("unknown key", vga::C_RED); }
                }
            } else { vga::term_puts("set: expected key=value", vga::C_RED); }
        }
        "run" => {
            if let Some(app) = registry.find(arg) {
                let app = app.clone();
                let ctx = apps::RuntimeCtx {
                    total_tokens: 0, tsc: math::rdtsc(),
                    nic: "", usb: "",
                    model_name: "", d_model: 0, n_layers: 0,
                };
                vga::term_puts("running ", vga::C_GREEN);
                vga::term_puts(&app.name, vga::C_GREEN);
                vga::term_puts(":\n", 0);
                apps::run_recipe(&app.recipe, &ctx);
            } else {
                vga::term_puts("no such app: ", vga::C_RED);
                vga::term_puts(arg, vga::C_RED);
            }
        }
        "clear" => {
            vga::clear_chat();
            vga::term_puts("cleared\n", vga::C_GREEN);
        }
        // Filesystem tool calls: <<TOOL: fs.save name body...>>
        //                       <<TOOL: fs.load name>>
        //                       <<TOOL: fs.list>>
        "fs.save" => {
            let (name, body) = match arg.find(char::is_whitespace) {
                Some(i) => (&arg[..i], arg[i..].trim_start()),
                None    => (arg, ""),
            };
            if name.is_empty() {
                vga::term_puts("fs.save: missing name", vga::C_RED);
            } else {
                match fs::save(name, body.as_bytes()) {
                    Ok(()) => { vga::term_puts("fs.save ", vga::C_GREEN); vga::term_puts(name, vga::C_GREEN); vga::term_puts(" ok", vga::C_GREEN); }
                    Err(e) => { vga::term_puts("fs.save error: ", vga::C_RED); vga::term_puts(e, vga::C_RED); }
                }
            }
        }
        "fs.load" => {
            if arg.is_empty() {
                vga::term_puts("fs.load: missing name", vga::C_RED);
            } else {
                match fs::load(arg) {
                    Ok(data) => {
                        vga::term_puts("fs.load ", vga::C_GREEN);
                        vga::term_puts(arg, vga::C_GREEN);
                        vga::term_puts(":\n", vga::C_DIM);
                        for &b in &data {
                            if (32..127).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t' {
                                vga::term_putc(b, vga::C_TEXT);
                            }
                        }
                    }
                    Err(e) => { vga::term_puts("fs.load error: ", vga::C_RED); vga::term_puts(e, vga::C_RED); }
                }
            }
        }
        "fs.list" => {
            match fs::list() {
                Ok(files) => {
                    vga::term_puts("fs.list:\n", vga::C_GREEN);
                    for (name, size) in &files {
                        vga::term_puts("  ", 0);
                        vga::term_puts(name, vga::C_GREEN);
                        vga::term_puts("  ", vga::C_DIM);
                        vga::term_putu32(*size, vga::C_ORANGE);
                        vga::term_puts(" bytes\n", vga::C_DIM);
                    }
                }
                Err(e) => { vga::term_puts("fs.list error: ", vga::C_RED); vga::term_puts(e, vga::C_RED); }
            }
        }
        _ => {
            vga::term_puts("unknown tool: ", vga::C_RED);
            vga::term_puts(verb, vga::C_RED);
        }
    }
}

// ── AI-driven app creation: chat proposes, code validates, loop ─────────────

const APP_GEN_SYSTEM: &str = "\
You are MatrixOS app generator.  Output ONLY a recipe in MatrixOS DSL, \
no explanation, no markdown fences.  Available commands (one per line):\n\
  say <text>      print text (may use $i $tokens $time $nic $usb $model)\n\
  info            print model + system info\n\
  time            print current TSC counter\n\
  tokens          print total tokens generated\n\
  nic             print NIC label\n\
  usb             print USB controller label\n\
  pause <N>       wait N units (0..50)\n\
  clear           clear chat scrollback\n\
  loop <N>        begin loop body, N <= 64, exposes $i\n\
  end             close loop\n\
Recipe must be 4–20 lines.  No comments, no quotes around the text.";

const APP_FIX_SYSTEM: &str = "\
You are a strict validator for the MatrixOS DSL.  The user gives you a \
broken recipe and the parser error.  Output ONLY the fixed recipe with \
NO explanation.  Use the same commands listed in the original spec.";

/// Run one turn through the given model, capturing the FULL response text.
/// Stops at EOS or `max_new` tokens.
fn ai_turn_capture(
    gguf:    &gguf::Gguf,
    cfg:     &model::ModelConfig,
    kv:      &mut model::KVCache,
    scratch: &mut model::ForwardScratch,
    tok:     &Option<tokenizer::Tokenizer>,
    system_text: &str,
    user_text:   &str,
    max_new:     usize,
) -> alloc::string::String {
    use alloc::string::String;
    // Reset KV so the new turn starts fresh.
    for v in kv.k.iter_mut() { *v = 0.0; }
    for v in kv.v.iter_mut() { *v = 0.0; }

    let mut prompt_tokens: alloc::vec::Vec<u32> = alloc::vec::Vec::new();
    if let Some(ref t) = *tok {
        t.build_prompt_with_system(system_text, user_text, &mut prompt_tokens);
    } else {
        for &b in user_text.as_bytes() { prompt_tokens.push(b as u32); }
    }

    let mut out = String::new();
    let mut tok_buf = [0u8; 32];
    model::generate(gguf, cfg, kv, scratch, &prompt_tokens, max_new, |id| {
        if let Some(ref t) = *tok {
            if let Some(s) = t.get_token(id as usize) {
                let n = tokenizer::decode_token_to_buf(s, &mut tok_buf);
                for i in 0..n {
                    out.push(tok_buf[i] as char);
                }
            }
        }
    });
    out
}

fn try_create_app_via_ai(
    arg: &str,
    chat_gguf: &gguf::Gguf, chat_cfg: &model::ModelConfig,
    chat_kv: &mut model::KVCache, chat_tok: &Option<tokenizer::Tokenizer>,
    code_gguf: &gguf::Gguf, code_cfg: &model::ModelConfig,
    code_kv: &mut model::KVCache, code_tok: &Option<tokenizer::Tokenizer>,
    scratch: &mut model::ForwardScratch,
    registry: &mut apps::Registry,
) {
    use alloc::string::String;

    let arg = arg.trim();
    if arg.is_empty() {
        vga::term_puts("usage: /create <name> <description>\n", vga::C_RED);
        return;
    }
    // First word = name, rest = description
    let (name, desc) = match arg.find(char::is_whitespace) {
        Some(i) => (&arg[..i], arg[i..].trim_start()),
        None    => (arg, ""),
    };
    if name.is_empty() {
        vga::term_puts("name required\n", vga::C_RED);
        return;
    }
    if registry.find(name).is_some() {
        vga::term_puts("name already exists. /delete or pick another.\n", vga::C_RED);
        return;
    }
    if desc.is_empty() {
        vga::term_puts("description required after name\n", vga::C_RED);
        return;
    }

    vga::term_puts("[create] app: ", vga::C_DIM);
    vga::term_puts(name, vga::C_GREEN);
    vga::term_puts("\n[create] description: ", vga::C_DIM);
    vga::term_puts(desc, vga::C_TEXT);
    vga::term_puts("\n", 0);

    // ── Round 1: chat model emits the initial recipe ─────────────────────
    vga::term_puts("[round 1] chat-AI proposing recipe...\n", vga::C_CYAN);
    let mut recipe_text: String = ai_turn_capture(
        chat_gguf, chat_cfg, chat_kv, scratch, chat_tok,
        APP_GEN_SYSTEM, desc, 256,
    );
    vga::term_puts("[round 1] draft (", vga::C_DIM);
    vga::term_putu32(recipe_text.len() as u32, vga::C_ORANGE);
    vga::term_puts(" bytes):\n", vga::C_DIM);
    for &b in recipe_text.as_bytes().iter().take(400) {
        vga::term_putc(b, vga::C_DIM);
    }
    vga::term_puts("\n", 0);

    // ── Rounds 2..=4: parse; if fail, ask code-AI to fix ─────────────────
    const MAX_ROUNDS: usize = 4;
    let mut author = apps::AppAuthor::AiChat;
    for round in 2..=MAX_ROUNDS {
        match apps::parse_recipe(&recipe_text) {
            Ok(cmds) if !cmds.is_empty() => {
                vga::term_puts("[ok] parsed ", vga::C_GREEN);
                vga::term_putu32(cmds.len() as u32, vga::C_ORANGE);
                vga::term_puts(" commands.\n", vga::C_GREEN);
                let app = apps::App {
                    name: String::from(name),
                    recipe: cmds,
                    author,
                    text: recipe_text.clone(),
                };
                match registry.register(app) {
                    Ok(()) => {
                        vga::term_puts("[done] app registered.\n", vga::C_GREEN);

                        // ── Auto-test: immediately run the registered app
                        //    so the user sees if it actually works, instead
                        //    of having to remember to /run it.  This is the
                        //    "iterate until done" UX the user asked for:
                        //    register + execute + show output in one shot.
                        if let Some(app) = registry.find(name) {
                            vga::term_puts("[test] auto-running ", vga::C_CYAN);
                            vga::term_puts(name, vga::C_GREEN);
                            vga::term_puts(":\n", vga::C_CYAN);
                            let ctx = apps::RuntimeCtx {
                                total_tokens: 0,
                                tsc: math::rdtsc(),
                                nic: "auto-test",
                                usb: "auto-test",
                                model_name: name,
                                d_model: 0,
                                n_layers: 0,
                            };
                            apps::run_recipe(&app.recipe, &ctx);
                            vga::term_puts("[test] done. /show ", vga::C_CYAN);
                            vga::term_puts(name, vga::C_GREEN);
                            vga::term_puts(" to see the recipe,  /delete ", vga::C_CYAN);
                            vga::term_puts(name, vga::C_GREEN);
                            vga::term_puts(" to remove.\n", vga::C_CYAN);
                        }
                    }
                    Err(e) => {
                        vga::term_puts("[fail] register: ", vga::C_RED);
                        vga::term_puts(e, vga::C_RED);
                        vga::term_puts("\n", 0);
                    }
                }
                return;
            }
            Ok(_) => {
                vga::term_puts("[warn] empty recipe, asking code-AI to retry\n", vga::C_ORANGE);
            }
            Err(e) => {
                vga::term_puts("[parse-error] line ", vga::C_RED);
                vga::term_putu32(e.line as u32, vga::C_RED);
                vga::term_puts(": ", vga::C_RED);
                vga::term_puts(e.msg, vga::C_RED);
                vga::term_puts("\n", 0);
            }
        }

        // Hand off to code-AI for a fix.
        vga::term_puts("[round ", vga::C_CYAN);
        vga::term_putu32(round as u32, vga::C_CYAN);
        vga::term_puts("] code-AI fixing...\n", vga::C_CYAN);

        // Compose the validator user-message.
        let mut fix_user = String::from("Broken recipe:\n");
        fix_user.push_str(&recipe_text);
        fix_user.push_str("\n\nRewrite the recipe correctly using only the listed commands.");

        recipe_text = ai_turn_capture(
            code_gguf, code_cfg, code_kv, scratch, code_tok,
            APP_FIX_SYSTEM, &fix_user, 320,
        );
        author = apps::AppAuthor::AiCode;
        vga::term_puts("[round ", vga::C_DIM);
        vga::term_putu32(round as u32, vga::C_DIM);
        vga::term_puts("] fix proposed (", vga::C_DIM);
        vga::term_putu32(recipe_text.len() as u32, vga::C_ORANGE);
        vga::term_puts(" bytes)\n", vga::C_DIM);
    }

    vga::term_puts("[give-up] could not produce a valid recipe in 4 rounds.\n", vga::C_RED);
    vga::term_puts("Try a simpler description, or write the recipe yourself.\n", vga::C_DIM);
}

/// Resolve a hostname to an IPv4 via the configured DNS server.
/// Uses the same e1000 Driver + ARP-of-gateway approach as /ping.
/// Returns Err on parse / timeout failures.
fn resolve_host(host: &str) -> Result<[u8; 4], &'static str> {
    let hhdm = unsafe { HHDM_OFFSET };
    let our_ip = unsafe { OUR_IP };
    let dns_ip = unsafe { DNS_IP };
    let gateway = unsafe { GATEWAY_IP };

    let nic = net::probe(hhdm).ok_or("no NIC")?;
    let mut drv = net::Driver::init(nic, hhdm);
    let mut cache = net::ArpCache::new();

    // ARP the gateway (or DNS server directly if on same subnet — we just
    // ARP the gateway since DNS replies traverse it anyway).
    let arp_target = gateway;
    drv.send_arp(our_ip, arp_target).map_err(|_| "ARP TX fail")?;
    let mut gw_mac: Option<[u8; 6]> = None;
    for _ in 0..400_000u32 {
        if let Some((idx, _)) = drv.poll_rx() {
            let f = drv.rx_frame(idx);
            if let Some(reply) = net::process_arp(f, &mut cache, drv.nic.mac, our_ip) {
                let _ = drv.send(&reply);
            }
            if let Some(m) = cache.lookup(arp_target) { gw_mac = Some(m); break; }
        }
        unsafe { core::arch::asm!("pause"); }
    }
    let gw_mac = gw_mac.ok_or("ARP timeout")?;

    // Build DNS query
    let xid: u16 = math::rdtsc() as u16;
    let mut dns_q = [0u8; 512];
    let dq_len = net::build_dns_query(host, xid, &mut dns_q).ok_or("DNS query encode failed")?;
    let mut frame = [0u8; 1500];
    let n = net::build_udp(
        drv.nic.mac, our_ip, 53_000 + (xid & 0x0FFF),
        gw_mac, dns_ip, net::DNS_PORT,
        &dns_q[..dq_len], &mut frame,
    ).ok_or("UDP build failed")?;
    drv.send(&frame[..n]).map_err(|_| "UDP TX fail")?;

    // Wait for reply
    for _ in 0..2_000_000u32 {
        if let Some((idx, _)) = drv.poll_rx() {
            let f = drv.rx_frame(idx);
            if let Some(pkt) = net::parse_udp(f) {
                if pkt.src_port == net::DNS_PORT {
                    if let Some(ip) = net::parse_dns_a_reply(pkt.payload, xid) {
                        return Ok(ip);
                    }
                }
            }
        }
        unsafe { core::arch::asm!("pause"); }
    }
    Err("DNS reply timeout")
}

/// Print an IPv4 as dotted-quad to the terminal.
fn print_ipv4(ip: [u8; 4]) {
    for (i, &b) in ip.iter().enumerate() {
        if i > 0 { vga::term_putc(b'.', vga::C_DIM); }
        vga::term_putu32(b as u32, vga::C_CYAN);
    }
}

/// Parse a dotted-quad IPv4 like "192.168.1.1" into [u8; 4].
fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let s = s.trim();
    let mut out = [0u8; 4];
    let mut idx = 0usize;
    let mut cur: u32 = 0;
    let mut seen_digit = false;
    for b in s.bytes() {
        match b {
            b'0'..=b'9' => {
                cur = cur.checked_mul(10)?.checked_add((b - b'0') as u32)?;
                if cur > 255 { return None; }
                seen_digit = true;
            }
            b'.' => {
                if !seen_digit || idx >= 3 { return None; }
                out[idx] = cur as u8;
                idx += 1;
                cur = 0;
                seen_digit = false;
            }
            _ => return None,
        }
    }
    if !seen_digit || idx != 3 { return None; }
    out[3] = cur as u8;
    Some(out)
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

    // CPUID-detect AVX2+FMA.  Enable OSXSAVE + XCR0 if available so we
    // can safely execute AVX/FMA instructions.  Sets math::AVX2_AVAILABLE.
    let has_avx2 = math::cpu_supports_avx2();
    if has_avx2 {
        unsafe { math::enable_avx(); }
        math::set_avx2_available(true);
        vga::term_puts("[OK] AVX2+FMA enabled (8-wide matmul)\n", vga::C_GREEN);
    } else {
        vga::term_puts("[OK] AVX2 not present — using SSE3 (4-wide)\n", vga::C_DIM);
    }

    // Initialise the RNG (probes CPUID for RDRAND, seeds fallback PRNG).
    rng::init();
    vga::term_puts(
        if rng::has_hardware() { "[OK] RNG (RDRAND hardware)\n" }
        else                  { "[OK] RNG (xorshift PRNG)\n" },
        vga::C_GREEN,
    );

    // Calibrate TSC via the 8254 PIT so /uptime and tok/s are honest.
    // Skipping calibration is OK — TSC_HZ defaults to 3 GHz.
    unsafe { math::calibrate_tsc_via_pit(); }
    vga::term_puts("[OK] TSC ", vga::C_GREEN);
    vga::term_putu32((math::tsc_hz() / 1_000_000) as u32, vga::C_ORANGE);
    vga::term_puts(" MHz\n", vga::C_DIM);

    // Remember TSC at boot for /uptime measurement.
    let boot_tsc: u64 = math::rdtsc();

    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 3, 0xFF, 0x80, 0x00); }

    unsafe { idt::init(); }
    vga::term_puts("[OK] IDT loaded\n", vga::C_GREEN);

    // Remap the 8259 PIC pair so IRQ vectors don't collide with CPU
    // exceptions, then unmask IRQ 1 (keyboard) and STI.  After this,
    // scancodes accumulate in the ring buffer rather than being missed
    // between polls (the "paste drops chars" issue).
    unsafe {
        irq::remap_pic();
        irq::arm_pit();
        irq::unmask_timer();
        irq::unmask_keyboard();
        irq::enable();
    }
    vga::term_puts("[OK] IRQ keyboard + PIT timer armed\n", vga::C_GREEN);

    // PS/2 mouse — best effort; if absent, we silently skip and continue.
    let mouse_ok = unsafe {
        let ok = mouse::init();
        if ok { mouse::unmask_irq12(); }
        ok
    };
    if mouse_ok {
        vga::term_puts("[OK] PS/2 mouse online (IRQ 12)\n", vga::C_GREEN);
    } else {
        vga::term_puts("[--] PS/2 mouse not detected\n", vga::C_DIM);
    }

    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 4, 0xFF, 0x00, 0xFF); }

    if !BASE_REVISION.is_supported() {
        vga::term_puts("[FATAL] Limine revision not supported\n", vga::C_RED);
        loop { unsafe { core::arch::asm!("hlt"); } }
    }

    // ── Stage 2: HHDM (needed before heap alloc from memmap) ─────────────
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
    unsafe { HHDM_OFFSET = hhdm_offset; }
    unsafe { stage_marker(fb_ptr, pitch, bpp, fb_w, 5, 0x80, 0x80, 0xFF); }

    // ── Stage 3: heap (allocate from Limine memmap, NOT static BSS) ─────
    allocator::init_with_memmap(hhdm_offset);
    let heap_mb = unsafe { allocator::HEAP_BYTES / (1024 * 1024) };
    vga::term_puts("[OK] Heap online (", vga::C_GREEN);
    vga::term_putu32(heap_mb as u32, vga::C_ORANGE);
    vga::term_puts(" MB from memmap)\n", vga::C_GREEN);

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
    // Bumped 128 → 512: at 128 tokens the response had to fit in ~80 tokens
    // after the prompt template, which forced ultra-short answers.  Memmap
    // heap means we have GBs of RAM to play with.
    const MAX_SEQ: usize = 512;
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
        isa:          if has_avx2 { "AVX2+FMA" } else { "SSE3" },
        heap_mb:      unsafe { (allocator::HEAP_BYTES / (1024 * 1024)) as u32 },
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

    // Try restoring saved state from disk.  Silently falls through if no
    // HDD or no valid MATRIXOS-ST magic.
    if let Some(s) = state::load(&mut registry) {
        max_new_chat = (s.max_chat as usize).clamp(32, 512);
        max_new_code = (s.max_code as usize).clamp(32, 640);
        // total_tokens is per-session, don't restore
        let _ = s.auto_pick;
        vga::term_puts("[state] restored apps + settings from disk\n", vga::C_DIM);
    }

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
                    keyboard::KEY_F3 => {
                        // F3 = swap to the OTHER model (model "dropdown" — one
                        // key to flip back and forth without thinking which is
                        // which.)
                        if active_slot == 0 {
                            active_slot = 1;
                            active_cfg  = code_cfg;
                            active_tok  = &code_tok;
                            active_gguf = &code_gguf;
                            active_name = code_display;
                            vga::set_active_mode(1, chat_display, code_display);
                            print_mode_switch("CODE", vga::C_ORANGE);
                        } else {
                            active_slot = 0;
                            active_cfg  = chat_cfg;
                            active_tok  = &chat_tok;
                            active_gguf = &chat_gguf;
                            active_name = chat_display;
                            vga::set_active_mode(0, chat_display, code_display);
                            print_mode_switch("CHAT", vga::C_CYAN);
                        }
                    }
                    keyboard::KEY_F4 => {
                        // F4 = wipe the INACTIVE model's KV cache (reclaim
                        // memory + reset stale context the user no longer
                        // wants).  Active model is left alone.
                        let kv = if active_slot == 0 { &mut code_kv } else { &mut chat_kv };
                        for v in kv.k.iter_mut() { *v = 0.0; }
                        for v in kv.v.iter_mut() { *v = 0.0; }
                        vga::term_puts("\n[SYS] other-model KV cleared\n", vga::C_DIM);
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

        // ── /create <name> <description>: AI-built app with code-validator ─
        if len > 8 && &buf[..8] == b"/create " {
            let arg = core::str::from_utf8(&buf[8..len]).unwrap_or("");
            try_create_app_via_ai(
                arg,
                &chat_gguf, &chat_cfg, &mut chat_kv, &chat_tok,
                &code_gguf, &code_cfg, &mut code_kv, &code_tok,
                &mut scratch,
                &mut registry,
            );
            vga::term_puts("\n", 0);
            continue;
        }

        // ── /install <slot> [confirm]: real disk image write ─────────────
        if len >= 8 && &buf[..8] == b"/install" {
            let arg = if len > 9 {
                core::str::from_utf8(&buf[9..len]).unwrap_or("")
            } else { "" };
            do_install(arg, mod_data);
            vga::term_puts("\n", 0);
            continue;
        }

        // ── /plan <natural-language>: User→LLM→Planner→Executor ─────────
        if len > 6 && &buf[..6] == b"/plan " {
            let arg = core::str::from_utf8(&buf[6..len]).unwrap_or("");
            do_plan(
                arg,
                &chat_gguf, &chat_cfg, &mut chat_kv, &chat_tok,
                &code_gguf, &code_cfg, &mut code_kv, &code_tok,
                &mut scratch,
                &mut registry,
                &mut max_new_chat,
                &mut max_new_code,
                &mut auto_pick,
            );
            vga::term_puts("\n", 0);
            continue;
        }

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
                boot_tsc,
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

        // Buffer the full response for post-processing tool calls.
        let mut response_buf: alloc::string::String = alloc::string::String::new();

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
                            response_buf.push(tok_buf[i] as char);
                        }
                    }
                } else {
                    vga::term_putu32(token_id, ai_color);
                    vga::term_putc(b' ', 0);
                }
                if total_tokens % 4 == 0 {
                    // Rolling tok/s: tokens since tsc_start ÷ elapsed seconds.
                    // Cheap because we already capture tsc_start at turn boot.
                    let dt = math::rdtsc().wrapping_sub(tsc_start);
                    let live_tps: u32 = if dt > 0 && turn_tokens > 0 {
                        let num = (turn_tokens as u64).saturating_mul(math::tsc_hz());
                        (num / dt) as u32
                    } else { 0 };
                    vga::update_status_tokens(total_tokens, live_tps);
                }
            },
        );

        // ── Tool-call post-processor ──────────────────────────────────────
        // The chat AI can include <<TOOL: ...>> markers in its reply.  Now
        // that streaming is done, scan the buffer for them and execute.
        let n_tools = execute_tool_calls(
            &response_buf,
            &mut registry,
            &mut max_new_chat,
            &mut max_new_code,
            &mut auto_pick,
        );
        if n_tools > 0 {
            vga::term_puts("\n[", vga::C_DIM);
            vga::term_putu32(n_tools, vga::C_GREEN);
            vga::term_puts(" tool call(s) executed]", vga::C_DIM);
        }

        // Compute tok/s using the calibrated TSC frequency (math::tsc_hz()).
        let tsc_end  = math::rdtsc();
        let delta    = tsc_end.wrapping_sub(tsc_start);
        let toks_per_sec: u32 = if delta > 0 && turn_tokens > 0 {
            let num = (turn_tokens as u64).saturating_mul(math::tsc_hz());
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
