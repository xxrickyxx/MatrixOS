//! IRQ subsystem: PIC remap + IRQ 1 keyboard handler + scancode ring buffer.
//!
//! Until now keyboard reads were polled from port 0x60 inside the REPL loop.
//! Under fast input (paste, computer-use) the PS/2 controller's 1-byte FIFO
//! got overwritten between polls and characters were dropped.
//!
//! With IRQs enabled we instead:
//!   1. Remap the 8259 PIC pair so IRQ 0..15 land at vectors 0x20..0x2F
//!      (avoiding clashes with CPU exception vectors 0..0x1F).
//!   2. Mask every IRQ except IRQ 1 (keyboard).  We don't want timer or
//!      anything else firing yet — no scheduler.
//!   3. Install an `extern "x86-interrupt"` handler at vector 0x21 that
//!      reads the scancode from 0x60, pushes into a 256-byte ring buffer,
//!      then sends EOI to the master PIC and returns.
//!   4. The keyboard module reads from the ring buffer instead of polling.

use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};

// ── PIC ports ───────────────────────────────────────────────────────────────
const PIC1_CMD:  u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD:  u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

const ICW1_INIT:    u8 = 0x10;
const ICW1_ICW4:    u8 = 0x01;
const ICW4_8086:    u8 = 0x01;
const PIC_EOI:      u8 = 0x20;

const KBD_DATA_PORT: u16 = 0x60;

#[inline(always)]
unsafe fn outb(port: u16, val: u8) {
    asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
}

#[inline(always)]
unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    asm!("in al, dx", in("dx") port, out("al") v, options(nomem, nostack, preserves_flags));
    v
}

/// I/O wait — write to an unused port; needed between PIC ICW writes on
/// some older chipsets.
#[inline(always)]
unsafe fn io_wait() {
    outb(0x80, 0);
}

/// Remap PIC IRQs 0..15 to interrupt vectors 0x20..0x2F.  After this,
/// IRQ 1 (keyboard) fires at vector 0x21.  Leaves all IRQs masked
/// — caller must `unmask_keyboard()` to let scancodes through.
pub unsafe fn remap_pic() {
    // Save the masks (we'll write fresh ones)
    let _m1 = inb(PIC1_DATA);
    let _m2 = inb(PIC2_DATA);

    // Start init sequence in cascade mode
    outb(PIC1_CMD, ICW1_INIT | ICW1_ICW4); io_wait();
    outb(PIC2_CMD, ICW1_INIT | ICW1_ICW4); io_wait();

    // ICW2: vector offsets
    outb(PIC1_DATA, 0x20); io_wait();   // master 0x20..0x27
    outb(PIC2_DATA, 0x28); io_wait();   // slave  0x28..0x2F

    // ICW3: cascade — tell master that slave is on IRQ 2
    outb(PIC1_DATA, 0x04); io_wait();
    outb(PIC2_DATA, 0x02); io_wait();

    // ICW4: 8086/88 mode
    outb(PIC1_DATA, ICW4_8086); io_wait();
    outb(PIC2_DATA, ICW4_8086); io_wait();

    // Mask EVERY IRQ initially — caller chooses which to unmask.
    outb(PIC1_DATA, 0xFF); io_wait();
    outb(PIC2_DATA, 0xFF); io_wait();
}

/// Unmask IRQ 1 on the master PIC so keyboard interrupts reach the CPU.
pub unsafe fn unmask_keyboard() {
    let cur = inb(PIC1_DATA);
    outb(PIC1_DATA, cur & !(1 << 1));
}

// ── Scancode ring buffer ────────────────────────────────────────────────────
//
// Single-producer (IRQ handler) / single-consumer (REPL polling) lock-free
// ring.  AtomicUsize indices, fixed power-of-two capacity for cheap modulo.

const RING_CAP: usize = 256;
static mut RING: [u8; RING_CAP] = [0; RING_CAP];
static HEAD: AtomicUsize = AtomicUsize::new(0);   // written by ISR
static TAIL: AtomicUsize = AtomicUsize::new(0);   // read by main code

/// Pop the next scancode if any, returning None if the ring is empty.
pub fn ring_pop() -> Option<u8> {
    let tail = TAIL.load(Ordering::Relaxed);
    let head = HEAD.load(Ordering::Acquire);
    if tail == head { return None; }
    let v = unsafe { RING[tail % RING_CAP] };
    TAIL.store(tail.wrapping_add(1), Ordering::Release);
    Some(v)
}

/// Push from ISR.  If the ring is full we drop the oldest entry (paste
/// faster than the user consumes shouldn't bring the keyboard down).
unsafe fn ring_push(b: u8) {
    let head = HEAD.load(Ordering::Relaxed);
    RING[head % RING_CAP] = b;
    HEAD.store(head.wrapping_add(1), Ordering::Release);
}

// ── The IRQ 1 handler ───────────────────────────────────────────────────────

#[repr(C)]
pub struct InterruptStackFrame {
    pub ip:     u64,
    pub cs:     u64,
    pub flags:  u64,
    pub sp:     u64,
    pub ss:     u64,
}

pub extern "x86-interrupt" fn keyboard_isr(_frame: InterruptStackFrame) {
    unsafe {
        // Read the scancode (also acks the PS/2 controller).
        let sc = inb(KBD_DATA_PORT);
        ring_push(sc);
        // EOI to master.
        outb(PIC1_CMD, PIC_EOI);
    }
}

/// Enable interrupts globally (after IDT + PIC are set up).
#[inline(always)]
pub unsafe fn enable() { asm!("sti", options(nomem, nostack, preserves_flags)); }

/// Disable interrupts globally.
#[inline(always)]
pub unsafe fn disable() { asm!("cli", options(nomem, nostack, preserves_flags)); }
