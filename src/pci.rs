//! PCI bus enumeration over the legacy CF8/CFC config space ports.
//!
//! This module walks bus 0 (the only bus VirtualBox typically exposes for
//! emulated hardware unless bridges are present) and reports every device,
//! flagging the ones whose `class_code` indicates a network controller
//! (PCI base class 0x02).  This is the bare minimum: it lists what NICs are
//! present so the OS — and the AI agents on top — can know what network
//! hardware is reachable, without yet driving it.

use core::arch::asm;

pub const PCI_CFG_ADDR: u16 = 0xCF8;
pub const PCI_CFG_DATA: u16 = 0xCFC;

#[inline(always)]
unsafe fn outl(port: u16, val: u32) {
    asm!("out dx, eax", in("dx") port, in("eax") val, options(nostack, nomem, preserves_flags));
}

#[inline(always)]
unsafe fn inl(port: u16) -> u32 {
    let v: u32;
    asm!("in eax, dx", in("dx") port, out("eax") v, options(nostack, nomem, preserves_flags));
    v
}

/// Read a 32-bit value from PCI config space.
fn pci_read32(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    let addr: u32 = 0x8000_0000
        | ((bus  as u32) << 16)
        | ((dev  as u32) << 11)
        | ((func as u32) << 8)
        | ((off  as u32) & 0xFC);
    unsafe {
        outl(PCI_CFG_ADDR, addr);
        inl(PCI_CFG_DATA)
    }
}

#[derive(Copy, Clone)]
pub struct PciDevice {
    pub bus:        u8,
    pub dev:        u8,
    pub func:       u8,
    pub vendor_id:  u16,
    pub device_id:  u16,
    pub class_code: u8,   // bits 24..32 of register 0x08
    pub subclass:   u8,
    pub prog_if:    u8,
    pub revision:   u8,
    pub header_typ: u8,
}

impl PciDevice {
    /// Returns true if this device is a network controller (PCI class 0x02).
    pub fn is_network(&self) -> bool { self.class_code == 0x02 }

    /// Best-effort vendor name for the most common emulated NICs.
    pub fn vendor_name(&self) -> &'static str {
        match self.vendor_id {
            0x8086 => "Intel",
            0x10EC => "Realtek",
            0x1022 => "AMD",
            0x1AF4 => "Virtio",      // VirtIO net (KVM/QEMU)
            0x80EE => "InnoTek",      // VirtualBox additions
            0x1B36 => "Red Hat",
            0x15AD => "VMware",
            _      => "Unknown",
        }
    }

    /// Best-effort NIC model name for known device IDs.
    pub fn nic_model(&self) -> &'static str {
        match (self.vendor_id, self.device_id) {
            (0x8086, 0x100E) => "Intel 82540EM (e1000)",
            (0x8086, 0x100F) => "Intel 82545EM (e1000)",
            (0x8086, 0x10D3) => "Intel 82574L  (e1000e)",
            (0x8086, 0x153A) => "Intel I217-LM",
            (0x8086, 0x15B7) => "Intel I219-LM",
            (0x10EC, 0x8139) => "Realtek RTL8139",
            (0x10EC, 0x8168) => "Realtek RTL8168 (r8169)",
            (0x10EC, 0x8125) => "Realtek RTL8125 (2.5G)",
            (0x1AF4, 0x1000) => "Virtio Network",
            (0x80EE, 0xBEEF) => "VBox HGCM",
            _ => "Network controller",
        }
    }
}

/// Walk bus 0 enumerating up to 32 devices, 8 functions each.
/// Calls `f(dev)` for every present device.
pub fn enumerate<F: FnMut(PciDevice)>(mut f: F) {
    for dev in 0..32u8 {
        let vd = pci_read32(0, dev, 0, 0x00);
        let vendor = (vd & 0xFFFF) as u16;
        if vendor == 0xFFFF { continue; }   // no device

        // Header type tells us if it's multifunction
        let bist_ht = pci_read32(0, dev, 0, 0x0C);
        let header_typ = ((bist_ht >> 16) & 0xFF) as u8;
        let multi = (header_typ & 0x80) != 0;

        let max_func = if multi { 8 } else { 1 };
        for func in 0..max_func {
            let vd2 = pci_read32(0, dev, func, 0x00);
            let vendor2 = (vd2 & 0xFFFF) as u16;
            if vendor2 == 0xFFFF { continue; }
            let device2 = ((vd2 >> 16) & 0xFFFF) as u16;
            let class_reg = pci_read32(0, dev, func, 0x08);
            let revision  = (class_reg        & 0xFF) as u8;
            let prog_if   = ((class_reg >>  8) & 0xFF) as u8;
            let subclass  = ((class_reg >> 16) & 0xFF) as u8;
            let class_code= ((class_reg >> 24) & 0xFF) as u8;
            let ht_reg    = pci_read32(0, dev, func, 0x0C);
            let header_typ= ((ht_reg >> 16) & 0xFF) as u8 & 0x7F;

            f(PciDevice {
                bus: 0, dev, func,
                vendor_id: vendor2,
                device_id: device2,
                class_code, subclass, prog_if, revision, header_typ,
            });
        }
    }
}

// ── NIC MAC address reader ────────────────────────────────────────────────────

/// Read a 32-bit value from a memory-mapped I/O address.
/// Used for register reads from NIC BARs.
#[inline(always)]
unsafe fn mmio_read32(addr: u64) -> u32 {
    core::ptr::read_volatile(addr as *const u32)
}

impl PciDevice {
    /// Read the 32-bit BAR at config-space offset `bar_off` (0x10, 0x14, …).
    /// Returns (base_address, is_memory_bar) — strips the type bits.
    pub fn read_bar32(&self, bar_off: u8) -> (u64, bool) {
        let raw = pci_read32(self.bus, self.dev, self.func, bar_off);
        let is_mem = (raw & 1) == 0;
        let base = if is_mem { (raw & 0xFFFF_FFF0) as u64 } else { (raw & 0xFFFF_FFFC) as u64 };
        (base, is_mem)
    }

    /// Read a 64-bit BAR pair (e.g. 64-bit memory BAR at `bar_off`/`bar_off+4`).
    pub fn read_bar64(&self, bar_off: u8) -> u64 {
        let lo = pci_read32(self.bus, self.dev, self.func, bar_off)     & 0xFFFF_FFF0;
        let hi = pci_read32(self.bus, self.dev, self.func, bar_off + 4) as u64;
        (hi << 32) | (lo as u64)
    }

    /// Try to read the NIC's MAC address.
    ///
    /// `hhdm_offset` is Limine's higher-half direct map offset: the BAR's
    /// physical address must be translated to virtual (phys + hhdm) before
    /// dereferencing, otherwise we'd hit an unmapped page and triple-fault.
    /// If `hhdm_offset == 0` we skip the read and return zeros.
    ///
    /// Works for Intel e1000/e1000e: RAL0 at BAR0+0x5400, RAH0 at BAR0+0x5404.
    /// Returns [u8;6] MAC or all-zero on failure.
    pub fn read_mac_e1000(&self, hhdm_offset: u64) -> [u8; 6] {
        if hhdm_offset == 0 { return [0u8; 6]; }

        let (bar0, is_mem) = self.read_bar32(0x10);
        if !is_mem || bar0 == 0 { return [0u8; 6]; }

        // Check for 64-bit BAR (type bits 2:1 == 0b10)
        let raw_bar0 = pci_read32(self.bus, self.dev, self.func, 0x10);
        let bar_type = (raw_bar0 >> 1) & 3;
        let base_phys: u64 = if bar_type == 2 {
            self.read_bar64(0x10)
        } else {
            bar0
        };
        if base_phys == 0 { return [0u8; 6]; }
        // Sanity: typical BARs are below 4 GiB or in the FE/FF range. Reject
        // anything that looks like a Limine virtual address already.
        if base_phys >= 0x0000_8000_0000_0000 { return [0u8; 6]; }

        // Translate physical → HHDM virtual.
        let base = base_phys.wrapping_add(hhdm_offset);

        // e1000: RAL0 = MAC bytes 0-3 (LE), RAH0 = bytes 4-5 (LE) + valid bit
        unsafe {
            let ral = mmio_read32(base + 0x5400);
            let rah = mmio_read32(base + 0x5404);
            // RAH valid bit may not be set in some virtualised NICs; accept
            // any non-broadcast, non-zero MAC.
            let mac = [
                (ral        & 0xFF) as u8,
                ((ral >>  8) & 0xFF) as u8,
                ((ral >> 16) & 0xFF) as u8,
                ((ral >> 24) & 0xFF) as u8,
                (rah        & 0xFF) as u8,
                ((rah >>  8) & 0xFF) as u8,
            ];
            // Reject all-0xFF (no device) and all-0x00 (invalid).
            if mac.iter().all(|&b| b == 0xFF) || mac.iter().all(|&b| b == 0) {
                return [0u8; 6];
            }
            mac
        }
    }
}

// ── Summary / probe ───────────────────────────────────────────────────────────

/// Summary returned by `probe_summary`.
pub struct PciSummary {
    pub total_devices: u32,
    pub nic_count:     u32,
    pub first_nic:     Option<PciDevice>,
    pub nic_mac:       [u8; 6],
}

/// Scan the bus and return aggregate counts + the first NIC found.
/// Also attempts to read the NIC's MAC address for Intel e1000 via HHDM-mapped MMIO.
/// Pass 0 for `hhdm_offset` to skip the MAC read (boot-safe fallback).
pub fn probe_summary(hhdm_offset: u64) -> PciSummary {
    let mut total = 0u32;
    let mut nics  = 0u32;
    let mut first_nic: Option<PciDevice> = None;
    enumerate(|d| {
        total += 1;
        if d.is_network() {
            nics += 1;
            if first_nic.is_none() { first_nic = Some(d); }
        }
    });
    let nic_mac = if let Some(ref n) = first_nic {
        match (n.vendor_id, n.device_id) {
            (0x8086, 0x100E) | (0x8086, 0x100F) | (0x8086, 0x10D3) => n.read_mac_e1000(hhdm_offset),
            _ => [0u8; 6],
        }
    } else { [0u8; 6] };
    PciSummary { total_devices: total, nic_count: nics, first_nic, nic_mac }
}
