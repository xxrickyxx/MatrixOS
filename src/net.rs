//! Intel e1000 / 82540EM minimal driver.
//!
//! Far from a full networking stack — what we do here:
//!   • Reach the NIC through its memory-mapped registers (BAR0, HHDM-translated)
//!   • Read MAC + STATUS to report link state, speed, full-duplex
//!   • Issue a soft reset (CTRL.RST) so a future TX/RX-ring patch can build
//!     on a known-clean state
//!
//! Full TX/RX with descriptor rings + ARP/DHCP is queued for a follow-up.

use crate::pci;

// ── e1000 MMIO register offsets ─────────────────────────────────────────────
const REG_CTRL:     u64 = 0x0000;   // Device control
const REG_STATUS:   u64 = 0x0008;   // Device status
const REG_EEPROM:   u64 = 0x0014;
const REG_CTRL_EXT: u64 = 0x0018;
const REG_ICR:      u64 = 0x00C0;
const REG_IMC:      u64 = 0x00D8;

const CTRL_RST:     u32 = 1 << 26;  // Software reset
const CTRL_FD:      u32 = 1 << 0;
const CTRL_SLU:     u32 = 1 << 6;   // Set link up

const STATUS_LU:    u32 = 1 << 1;   // Link Up
const STATUS_FD:    u32 = 1 << 0;   // Full Duplex
const STATUS_SPEED_MASK: u32 = 3 << 6;
const STATUS_SPEED_10:   u32 = 0 << 6;
const STATUS_SPEED_100:  u32 = 1 << 6;
const STATUS_SPEED_1000: u32 = 2 << 6;

pub struct NicInfo {
    pub addr:        pci::PciDevice,
    pub mmio_phys:   u64,
    pub mmio_virt:   u64,
    pub mac:         [u8; 6],
    pub link_up:     bool,
    pub speed_mbps:  u32,
    pub full_duplex: bool,
}

#[inline(always)]
unsafe fn mmio_r32(addr: u64) -> u32 { core::ptr::read_volatile(addr as *const u32) }

#[inline(always)]
unsafe fn mmio_w32(addr: u64, v: u32) { core::ptr::write_volatile(addr as *mut u32, v) }

/// Look at the first network controller from PCI and, if it's a recognised
/// Intel e1000 / 82540 / 82574 family, map BAR0 through HHDM and probe it.
///
/// `hhdm_offset` is Limine's HHDM base.  Pass 0 to skip probing entirely.
pub fn probe(hhdm_offset: u64) -> Option<NicInfo> {
    if hhdm_offset == 0 { return None; }

    // Find an e1000-class NIC.
    let mut found: Option<pci::PciDevice> = None;
    pci::enumerate(|d| {
        if d.is_network() && d.vendor_id == 0x8086 {
            match d.device_id {
                0x100E | 0x100F | 0x10D3 | 0x153A | 0x15B7 => {
                    if found.is_none() { found = Some(d); }
                }
                _ => {}
            }
        }
    });
    let nic = found?;

    // Read BAR0.
    let raw0 = pci::pci_read32(nic.bus, nic.dev, nic.func, 0x10);
    if raw0 & 1 != 0 { return None; } // I/O BAR — we want memory
    let bar_type = (raw0 >> 1) & 3;
    let base_phys: u64 = if bar_type == 2 {
        nic.read_bar64(0x10)
    } else {
        (raw0 & 0xFFFF_FFF0) as u64
    };
    if base_phys == 0 || base_phys >= 0x0000_8000_0000_0000 { return None; }
    let base = base_phys.wrapping_add(hhdm_offset);

    // Read MAC from RAL0/RAH0 (same as PciDevice::read_mac_e1000 but inline so
    // we don't probe twice).
    let ral = unsafe { mmio_r32(base + 0x5400) };
    let rah = unsafe { mmio_r32(base + 0x5404) };
    let mac = [
        (ral        & 0xFF) as u8,
        ((ral >>  8) & 0xFF) as u8,
        ((ral >> 16) & 0xFF) as u8,
        ((ral >> 24) & 0xFF) as u8,
        (rah        & 0xFF) as u8,
        ((rah >>  8) & 0xFF) as u8,
    ];

    // Read link status.
    let status = unsafe { mmio_r32(base + REG_STATUS) };
    let link_up = status & STATUS_LU != 0;
    let full_duplex = status & STATUS_FD != 0;
    let speed_mbps = match status & STATUS_SPEED_MASK {
        STATUS_SPEED_10   => 10,
        STATUS_SPEED_100  => 100,
        STATUS_SPEED_1000 => 1000,
        _ => 0,
    };

    Some(NicInfo {
        addr: nic,
        mmio_phys: base_phys,
        mmio_virt: base,
        mac,
        link_up,
        speed_mbps,
        full_duplex,
    })
}

/// Soft-reset the controller via CTRL.RST.  Spec says wait at least 1 µs;
/// we busy-wait a few hundred reads of STATUS which is plenty on real HW.
pub unsafe fn soft_reset(nic: &NicInfo) {
    let ctrl = mmio_r32(nic.mmio_virt + REG_CTRL);
    mmio_w32(nic.mmio_virt + REG_CTRL, ctrl | CTRL_RST);
    for _ in 0..1024 { let _ = mmio_r32(nic.mmio_virt + REG_STATUS); }
    // Disable all interrupts (we don't have an IRQ handler yet).
    mmio_w32(nic.mmio_virt + REG_IMC, 0xFFFF_FFFF);
    let _ = mmio_r32(nic.mmio_virt + REG_ICR);  // clear pending
    // Re-arm link.
    let ctrl = mmio_r32(nic.mmio_virt + REG_CTRL);
    mmio_w32(nic.mmio_virt + REG_CTRL, ctrl | CTRL_SLU | CTRL_FD);
}
