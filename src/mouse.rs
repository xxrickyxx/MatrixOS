//! PS/2 mouse driver — IRQ 12, 3-byte packet stream, on-screen cursor.
//!
//! The 8042 PS/2 controller exposes two channels: keyboard (already handled
//! by irq.rs) and "aux" (mouse).  We enable aux, send the standard reset +
//! defaults + enable-data-reporting sequence to the mouse, then assemble
//! incoming packets in the IRQ 12 handler.
//!
//! On each complete 3-byte packet we update the cursor's (x, y) position
//! and (lazily) redraw a 12×12 white square at that position.  The previous
//! cursor square is erased by tracking the prior (x, y) and refilling that
//! area with the framebuffer's background colour.
//!
//! This is purely visual for now — apps don't yet receive clicks.  Future
//! PR: dispatch (x, y, buttons) into a global mouse-event queue the REPL
//! can pull from.

use core::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use core::arch::asm;
use crate::irq::InterruptStackFrame;

// ── 8042 ports & commands ───────────────────────────────────────────────────
const PS2_DATA: u16 = 0x60;
const PS2_CMD:  u16 = 0x64;
const PS2_STAT: u16 = 0x64;

const STAT_OUT_FULL: u8 = 1 << 0;
const STAT_IN_FULL:  u8 = 1 << 1;

#[inline(always)] unsafe fn inb(p: u16) -> u8 {
    let v: u8; asm!("in al, dx", in("dx") p, out("al") v, options(nomem, nostack, preserves_flags)); v
}
#[inline(always)] unsafe fn outb(p: u16, v: u8) {
    asm!("out dx, al", in("dx") p, in("al") v, options(nomem, nostack, preserves_flags));
}

unsafe fn wait_in() {
    for _ in 0..100_000 {
        if inb(PS2_STAT) & STAT_IN_FULL == 0 { return; }
    }
}

unsafe fn wait_out() {
    for _ in 0..100_000 {
        if inb(PS2_STAT) & STAT_OUT_FULL != 0 { return; }
    }
}

/// Send a command byte to the 8042 itself (writes to 0x64).
unsafe fn cmd(byte: u8) { wait_in(); outb(PS2_CMD, byte); }
/// Write a byte to the mouse device (prefix 0xD4 to tell 8042 to route).
unsafe fn write_mouse(byte: u8) {
    cmd(0xD4);
    wait_in(); outb(PS2_DATA, byte);
}
/// Block-read a byte from the mouse (waits OBF).
unsafe fn read_mouse() -> u8 { wait_out(); inb(PS2_DATA) }

// ── Public init ─────────────────────────────────────────────────────────────

/// Set up the mouse: enable aux, set defaults, enable streaming.
/// Returns false if the device doesn't ack — the slot might be empty.
pub unsafe fn init() -> bool {
    // Enable aux port
    cmd(0xA8);
    // Enable interrupt for the aux device by setting bit 1 of the controller
    // configuration byte.  Read it via 0x20, write back via 0x60.
    cmd(0x20);
    let mut conf = read_mouse();
    conf |= 1 << 1;     // enable IRQ12
    conf &= !(1 << 5);  // make sure aux clock is enabled
    cmd(0x60);
    wait_in(); outb(PS2_DATA, conf);

    // Reset (0xFF): expect 0xFA ack, then 0xAA self-test ok, then device-id 0
    write_mouse(0xFF);
    let a = read_mouse();
    if a != 0xFA { return false; }
    let _bat = read_mouse();
    let _id  = read_mouse();

    // Set defaults (0xF6)
    write_mouse(0xF6);
    let _ = read_mouse();

    // Enable data reporting (0xF4)
    write_mouse(0xF4);
    let _ = read_mouse();

    true
}

// ── Packet assembly + cursor state ──────────────────────────────────────────

static PACKET_PHASE: AtomicU8 = AtomicU8::new(0);
static PACKET_B0:    AtomicU8 = AtomicU8::new(0);
static PACKET_B1:    AtomicU8 = AtomicU8::new(0);

static MOUSE_X: AtomicI32 = AtomicI32::new(512);
static MOUSE_Y: AtomicI32 = AtomicI32::new(384);
static BUTTONS: AtomicU8  = AtomicU8::new(0);

pub fn position() -> (i32, i32) {
    (MOUSE_X.load(Ordering::Relaxed), MOUSE_Y.load(Ordering::Relaxed))
}
pub fn buttons() -> u8 { BUTTONS.load(Ordering::Relaxed) }

/// IRQ 12 handler — chained through slave PIC so EOI both PICs.
pub extern "x86-interrupt" fn mouse_isr(_frame: InterruptStackFrame) {
    unsafe {
        // First sanity-check that bit 5 of the 8042 status is set (data is
        // from the aux device).  If not, drop the byte to keep keyboard
        // IRQ-1 path clean.
        let status = inb(PS2_STAT);
        if status & (1 << 5) == 0 {
            // EOI then bail.
            outb(0xA0, 0x20);
            outb(0x20, 0x20);
            return;
        }
        let b = inb(PS2_DATA);

        let phase = PACKET_PHASE.load(Ordering::Relaxed);
        match phase {
            0 => {
                // First byte should have bit 3 set per PS/2 spec.  If not,
                // we're out of sync — just drop this byte.
                if b & 0x08 != 0 {
                    PACKET_B0.store(b, Ordering::Relaxed);
                    PACKET_PHASE.store(1, Ordering::Relaxed);
                }
            }
            1 => {
                PACKET_B1.store(b, Ordering::Relaxed);
                PACKET_PHASE.store(2, Ordering::Relaxed);
            }
            _ => {
                let b0 = PACKET_B0.load(Ordering::Relaxed);
                let b1 = PACKET_B1.load(Ordering::Relaxed);
                let b2 = b;
                PACKET_PHASE.store(0, Ordering::Relaxed);

                // Decode movement (9-bit signed Δ, sign in b0).
                let mut dx = b1 as i32;
                if b0 & 0x10 != 0 { dx -= 256; }
                let mut dy = b2 as i32;
                if b0 & 0x20 != 0 { dy -= 256; }
                // Y is "up positive" in PS/2 — invert for screen coords.
                let dy = -dy;

                BUTTONS.store(b0 & 0x07, Ordering::Relaxed);

                let cur_x = MOUSE_X.load(Ordering::Relaxed) + dx;
                let cur_y = MOUSE_Y.load(Ordering::Relaxed) + dy;
                // Clamp to a reasonable framebuffer — main passes the real
                // bounds via tick() below, but ISR doesn't know them.
                let cur_x = cur_x.clamp(0, 4096);
                let cur_y = cur_y.clamp(0, 4096);
                MOUSE_X.store(cur_x, Ordering::Relaxed);
                MOUSE_Y.store(cur_y, Ordering::Relaxed);
            }
        }

        // EOI to slave AND master (IRQ 12 is on slave PIC).
        outb(0xA0, 0x20);
        outb(0x20, 0x20);
    }
}

/// Unmask IRQ 12 on the slave PIC (and the cascade bit on master).
pub unsafe fn unmask_irq12() {
    // Master: unmask the cascade (IRQ 2)
    let m = inb(0x21);
    outb(0x21, m & !(1 << 2));
    // Slave: unmask IRQ 12 (bit 4 of slave PIC = IRQ 12)
    let s = inb(0xA1);
    outb(0xA1, s & !(1 << 4));
}

// ── Cursor rendering ────────────────────────────────────────────────────────
//
// Caller (REPL loop) invokes `draw_cursor()` periodically.  We track the
// previous (x, y) so we can erase before redrawing.

static mut PREV_X: i32 = -1;
static mut PREV_Y: i32 = -1;

/// Draw the cursor at the current mouse position, erasing the previous one.
pub fn draw_cursor(bg: u32) {
    use crate::vga;
    unsafe {
        if vga::FB_PTR.is_null() { return; }
        let w = vga::FB_WIDTH  as i32;
        let h = vga::FB_HEIGHT as i32;
        let x = MOUSE_X.load(Ordering::Relaxed).clamp(0, w - 12);
        let y = MOUSE_Y.load(Ordering::Relaxed).clamp(0, h - 12);
        if x == PREV_X && y == PREV_Y { return; }
        // Erase old position
        if PREV_X >= 0 && PREV_Y >= 0 {
            vga::fill_rect(PREV_X as usize, PREV_Y as usize, 12, 12, bg);
        }
        // Draw new
        vga::fill_rect(x as usize, y as usize, 12, 12, 0xFFFFFF);
        PREV_X = x;
        PREV_Y = y;
    }
}
