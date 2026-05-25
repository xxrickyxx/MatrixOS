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

/// Summary returned by `probe_summary`.
pub struct PciSummary {
    pub total_devices: u32,
    pub nic_count:     u32,
    pub first_nic:     Option<PciDevice>,
}

/// Scan the bus and return aggregate counts + the first NIC found.
pub fn probe_summary() -> PciSummary {
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
    PciSummary { total_devices: total, nic_count: nics, first_nic }
}
