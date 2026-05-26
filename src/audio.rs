//! PC-speaker beep via PIT channel 2.
//!
//! The 8254 PIT has three channels.  Channel 0 drives our 100 Hz timer
//! IRQ (see irq.rs).  Channel 2 is wired through port 0x61 to the PC
//! speaker, so we can program it as a square-wave generator and toggle
//! bit 0 of port 0x61 to enable the speaker.  Frequency = 1_193_182 / div.
//!
//! `beep_blocking(freq_hz, ms)` is a synchronous busy-wait — uses the
//! PIT-driven tick_ms() for the duration.

use core::arch::asm;
use crate::irq;

const PIT_CMD:  u16 = 0x43;
const PIT_CH2:  u16 = 0x42;
const SPK_PORT: u16 = 0x61;

#[inline(always)] unsafe fn inb(p: u16) -> u8 {
    let v: u8; asm!("in al, dx", in("dx") p, out("al") v, options(nomem, nostack, preserves_flags)); v
}
#[inline(always)] unsafe fn outb(p: u16, v: u8) {
    asm!("out dx, al", in("dx") p, in("al") v, options(nomem, nostack, preserves_flags));
}

/// Make a beep of `freq_hz` for `ms` milliseconds.  Returns when the beep
/// has been silenced.  Frequencies outside ~37..32767 Hz get clamped.
pub fn beep_blocking(freq_hz: u32, ms: u32) {
    let f = freq_hz.clamp(37, 32767);
    let div: u16 = (1_193_182u32 / f) as u16;
    unsafe {
        // Program PIT channel 2: mode 3 (square wave), access lobyte/hibyte
        outb(PIT_CMD, 0b10_11_011_0);
        outb(PIT_CH2, (div & 0xFF) as u8);
        outb(PIT_CH2, (div >> 8)   as u8);
        // Enable speaker: bit 0 (gate) + bit 1 (speaker data)
        let prev = inb(SPK_PORT);
        outb(SPK_PORT, prev | 0b11);

        // Wait ms via the PIT-driven timer.
        let until = irq::tick_ms() + ms as u64;
        while irq::tick_ms() < until {
            asm!("pause");
        }

        // Silence: restore prev.
        outb(SPK_PORT, prev & !0b11);
    }
}
