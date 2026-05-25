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
pub mod ata;
pub mod state;
pub mod net;

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
    let mut dirty = true;

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
            if sc & 0x80 != 0 && sc != 0xAA && sc != 0xB6 { continue; }
            if let Some(ch) = kb.scancode_to_char(sc) {
                match ch {
                    '\n' => {
                        vga::draw_input_line(&[], 0);
                        return LineEvent::Line(len);
                    }
                    '\x08' => {
                        if len > 0 { len -= 1; dirty = true; }
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
                            dirty = true;
                        }
                    }
                }
            }
        }
        if !produced_something {
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
            vga::term_puts("  /install <slot>    dry-run install probe\n", vga::C_DIM);
            vga::term_puts("  /sysinfo           print system stats\n", vga::C_DIM);
            vga::term_puts("  /set max_chat=N    chat response length (32..512)\n", vga::C_DIM);
            vga::term_puts("  /set max_code=N    code response length (32..640)\n", vga::C_DIM);
            vga::term_puts("  /set auto=on|off   intent auto-switching\n", vga::C_DIM);
            vga::term_puts("  /save              persist apps + settings to disk\n", vga::C_DIM);
            vga::term_puts("  /load              read saved state from disk\n", vga::C_DIM);
            vga::term_puts("  /net [reset]       NIC info / link state / soft-reset\n", vga::C_DIM);
            vga::term_puts("  /meminfo           heap used / free\n", vga::C_DIM);
            vga::term_puts("  /uptime            seconds since boot (TSC)\n", vga::C_DIM);
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
                if ahci_count > 0 {
                    vga::term_puts("  VM controller is AHCI — switch to legacy IDE in VirtualBox\n", vga::C_ORANGE);
                    vga::term_puts("  (Settings -> Storage -> Controller: IDE, attach disk to it).\n", vga::C_ORANGE);
                } else {
                    vga::term_puts("  Attach an IDE hard disk in the VM settings.\n", vga::C_DIM);
                }
            } else {
                vga::term_puts("Use /install <slot> to install MatrixOS (dry-run).\n", vga::C_DIM);
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
            let hhdm = unsafe { HHDM_OFFSET };
            let do_reset = arg == "reset";
            match net::probe(hhdm) {
                Some(mut nic) => {
                    vga::term_puts("NIC: ", vga::C_CYAN);
                    vga::term_puts(nic.addr.nic_model(), vga::C_GREEN);
                    vga::term_puts("\n  MAC:   ", vga::C_DIM);
                    for (i, &b) in nic.mac.iter().enumerate() {
                        if i > 0 { vga::term_putc(b':', vga::C_DIM); }
                        vga::term_puthex8(b, vga::C_CYAN);
                    }
                    vga::term_puts("\n  BAR0:  0x", vga::C_DIM);
                    // print high+low 32-bit halves
                    vga::term_puthex8((nic.mmio_phys >> 56) as u8, vga::C_ORANGE);
                    vga::term_puthex8((nic.mmio_phys >> 48) as u8, vga::C_ORANGE);
                    vga::term_puthex8((nic.mmio_phys >> 40) as u8, vga::C_ORANGE);
                    vga::term_puthex8((nic.mmio_phys >> 32) as u8, vga::C_ORANGE);
                    vga::term_puthex8((nic.mmio_phys >> 24) as u8, vga::C_ORANGE);
                    vga::term_puthex8((nic.mmio_phys >> 16) as u8, vga::C_ORANGE);
                    vga::term_puthex8((nic.mmio_phys >>  8) as u8, vga::C_ORANGE);
                    vga::term_puthex8( nic.mmio_phys        as u8, vga::C_ORANGE);
                    vga::term_puts("\n  link:  ", vga::C_DIM);
                    if nic.link_up {
                        vga::term_puts("UP", vga::C_GREEN);
                        vga::term_puts("  ", 0);
                        vga::term_putu32(nic.speed_mbps, vga::C_ORANGE);
                        vga::term_puts(" Mbps", vga::C_DIM);
                        vga::term_puts(if nic.full_duplex {" full-dup\n"} else {" half-dup\n"}, vga::C_DIM);
                    } else {
                        vga::term_puts("DOWN", vga::C_RED);
                        vga::term_puts(" (cable / VM NAT may not provide link state)\n", vga::C_DIM);
                    }
                    if do_reset {
                        vga::term_puts("  resetting controller...\n", vga::C_CYAN);
                        unsafe { net::soft_reset(&nic); }
                        // Re-probe to see post-reset state.
                        if let Some(after) = net::probe(hhdm) {
                            nic = after;
                            vga::term_puts("  post-reset link: ", vga::C_DIM);
                            vga::term_puts(if nic.link_up {"UP\n"} else {"DOWN\n"},
                                           if nic.link_up {vga::C_GREEN} else {vga::C_RED});
                        }
                    } else {
                        vga::term_puts("  hint: /net reset  to issue CTRL.RST\n", vga::C_DIM);
                    }
                    vga::term_puts("  (TX/RX rings + packet send: future PR)\n", vga::C_DIM);
                }
                None => {
                    vga::term_puts("No e1000-class NIC found, or HHDM unavailable.\n", vga::C_RED);
                    vga::term_puts("Run /sysinfo to see what NIC the PCI scan reported.\n", vga::C_DIM);
                }
            }
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
            let elapsed_ticks = tsc.wrapping_sub(boot_tsc);
            let hz   = math::tsc_hz();
            let secs = elapsed_ticks / hz;
            let ms   = (elapsed_ticks % hz) * 1000 / hz;
            vga::term_puts("Uptime: ", vga::C_DIM);
            vga::term_putu32(secs as u32, vga::C_GREEN);
            vga::term_puts(".", vga::C_DIM);
            if ms < 100 { vga::term_putc(b'0', vga::C_GREEN); }
            if ms <  10 { vga::term_putc(b'0', vga::C_GREEN); }
            vga::term_putu32(ms as u32, vga::C_GREEN);
            vga::term_puts(" s  TSC=", vga::C_DIM);
            vga::term_putu32((hz / 1_000_000) as u32, vga::C_ORANGE);
            vga::term_puts(" MHz\n", vga::C_DIM);
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

fn do_install(arg: &str, mod_data: &[u8]) {
    let arg = arg.trim();
    if arg.is_empty() {
        vga::term_puts("usage: /install <slot>           dry-run\n", vga::C_RED);
        vga::term_puts("       /install <slot> confirm   actually write\n", vga::C_RED);
        return;
    }
    let (slot_s, rest) = match arg.find(char::is_whitespace) {
        Some(i) => (&arg[..i], arg[i..].trim_start()),
        None    => (arg, ""),
    };
    let slot = match parse_u32_safe(slot_s) {
        Some(n) if (n as usize) < 4 => n as usize,
        _ => { vga::term_puts("bad slot (0..3)\n", vga::C_RED); return; }
    };
    let confirm = rest == "confirm";

    let probe = ata::probe_all();
    let info = match &probe.drives[slot] {
        Some(d) => d,
        None    => { vga::term_puts("slot empty — run /disks\n", vga::C_RED); return; }
    };
    if info.is_atapi {
        vga::term_puts("target is ATAPI (CD/DVD), refusing to install there.\n", vga::C_RED);
        return;
    }
    let needed_bytes = mod_data.len() + 4096 * 512;   // bundle + reserved kernel area
    let avail_bytes  = (info.sectors_28 as u64) * 512;
    let target_label = info.addr.label();
    let target_model = info.model_str();
    let target_addr  = info.addr;

    vga::term_puts("[install] target:  ", vga::C_DIM);
    vga::term_puts(target_label, vga::C_GREEN);
    vga::term_puts("  ", 0);
    vga::term_puts(target_model, vga::C_TEXT);
    vga::term_puts("\n[install] size:    ", vga::C_DIM);
    vga::term_putu32(info.capacity_mb(), vga::C_ORANGE);
    vga::term_puts(" MB\n[install] need:    ", vga::C_DIM);
    vga::term_putu32((needed_bytes / (1024 * 1024)) as u32, vga::C_ORANGE);
    vga::term_puts(" MB (kernel + bundle)\n", vga::C_DIM);

    if (needed_bytes as u64) > avail_bytes {
        vga::term_puts("[install] target is too small. abort.\n", vga::C_RED);
        return;
    }

    // Always read sector 0 first to confirm the channel works.
    let mut mbr = [0u8; 512];
    if let Err(e) = ata::read_sectors(target_addr, 0, 1, &mut mbr) {
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
        vga::term_puts("[install] dry-run only. To actually write:\n", vga::C_ORANGE);
        vga::term_puts("  /install ", vga::C_ORANGE);
        vga::term_puts(slot_s, vga::C_ORANGE);
        vga::term_puts(" confirm\n", vga::C_ORANGE);
        return;
    }

    // ── Step 1: write the MatrixOS MBR header to LBA 0 ───────────────────
    vga::term_puts("[install] writing MBR... ", vga::C_CYAN);
    let mut hdr = [0u8; 512];
    hdr[..8].copy_from_slice(b"MATRIXOS");
    hdr[8..12].copy_from_slice(&1u32.to_le_bytes());                 // version
    hdr[12..16].copy_from_slice(&(mod_data.len() as u32).to_le_bytes()); // bundle size
    hdr[16..20].copy_from_slice(&4097u32.to_le_bytes());             // bundle LBA
    hdr[20..24].copy_from_slice(&1u32.to_le_bytes());                // kernel LBA
    hdr[510] = 0x55;
    hdr[511] = 0xAA;
    match ata::write_sectors(target_addr, 0, 1, &hdr) {
        Ok(()) => vga::term_puts("OK\n", vga::C_GREEN),
        Err(e) => {
            vga::term_puts("FAIL: ", vga::C_RED);
            vga::term_puts(e, vga::C_RED);
            vga::term_puts("\n", 0);
            return;
        }
    }

    // ── Step 2: stream the model bundle to disk ──────────────────────────
    vga::term_puts("[install] writing bundle (", vga::C_CYAN);
    vga::term_putu32((mod_data.len() / (1024 * 1024)) as u32, vga::C_ORANGE);
    vga::term_puts(" MB) — do not power off\n", vga::C_CYAN);

    // Save current cursor so the progress bar overwrites itself.
    vga::term_puts("[install] progress: ", vga::C_DIM);
    let mut last_pct: u32 = 0;
    match ata::install_image(target_addr, 4097, mod_data, |w, t| {
        let pct = ((w as u64) * 100 / (t.max(1) as u64)) as u32;
        if pct != last_pct {
            // Update an in-place 3-digit percentage in the status bar.
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

    // ── Step 3: verify by reading MBR back ───────────────────────────────
    let mut verify = [0u8; 512];
    if ata::read_sectors(target_addr, 0, 1, &mut verify).is_ok()
        && &verify[..8] == b"MATRIXOS"
    {
        vga::term_puts("[install] verified MBR magic. INSTALL COMPLETE.\n", vga::C_GREEN);
        vga::term_puts("[install] (bootloader stub not yet present — disk holds the\n", vga::C_DIM);
        vga::term_puts("[install] kernel + bundle but BIOS boot needs a future fix.)\n", vga::C_DIM);
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
                    vga::update_status_tokens(total_tokens, 0);
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
