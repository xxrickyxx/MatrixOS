//! Intel e1000 / 82540EM minimal driver.
//!
//! What this module does:
//!   • probe(): finds an Intel e1000 NIC, reads MAC + link state via MMIO
//!   • soft_reset(): CTRL.RST + mask all IRQs (we have no interrupt handler)
//!   • Driver::init(): sets up TX (8 descriptors) and RX (16 descriptors,
//!     2 KB buffers each) descriptor rings via DMA-able heap allocations
//!   • Driver::send_arp(): builds and transmits a broadcast ARP request
//!   • Driver::poll_rx(): drains any pending RX descriptors, returning the
//!     payload for the caller to inspect
//!
//! DMA addresses: our heap lives in Limine's HHDM region — meaning every
//! virtual address there is `phys + HHDM_OFFSET`.  We give the NIC the
//! physical address (virt − hhdm) so its built-in DMA engine reads/writes
//! the same memory we touch from the CPU.
//!
//! Not (yet) implemented: ARP-reply parsing into a neighbour table, DHCP,
//! IP/UDP/TCP, the kernel-side IRQ for RX notification.  Build-out from
//! this skeleton.

extern crate alloc;
use alloc::vec::Vec;
use crate::pci;

// ── e1000 MMIO register offsets ─────────────────────────────────────────────
const REG_CTRL:     u64 = 0x0000;
const REG_STATUS:   u64 = 0x0008;
const REG_ICR:      u64 = 0x00C0;
const REG_IMC:      u64 = 0x00D8;

// TX
const REG_TDBAL:    u64 = 0x3800;
const REG_TDBAH:    u64 = 0x3804;
const REG_TDLEN:    u64 = 0x3808;
const REG_TDH:      u64 = 0x3810;
const REG_TDT:      u64 = 0x3818;
const REG_TCTL:     u64 = 0x0400;
const REG_TIPG:     u64 = 0x0410;

// RX
const REG_RDBAL:    u64 = 0x2800;
const REG_RDBAH:    u64 = 0x2804;
const REG_RDLEN:    u64 = 0x2808;
const REG_RDH:      u64 = 0x2810;
const REG_RDT:      u64 = 0x2818;
const REG_RCTL:     u64 = 0x0100;

const CTRL_RST:     u32 = 1 << 26;
const CTRL_FD:      u32 = 1 << 0;
const CTRL_SLU:     u32 = 1 << 6;

const STATUS_LU:    u32 = 1 << 1;
const STATUS_FD:    u32 = 1 << 0;
const STATUS_SPEED_MASK: u32 = 3 << 6;
const STATUS_SPEED_10:   u32 = 0 << 6;
const STATUS_SPEED_100:  u32 = 1 << 6;
const STATUS_SPEED_1000: u32 = 2 << 6;

// TX descriptor command byte
const TX_CMD_EOP: u8 = 1 << 0;   // End Of Packet
const TX_CMD_IFCS: u8 = 1 << 1;  // Insert FCS
const TX_CMD_RS:  u8 = 1 << 3;   // Report Status

// TX descriptor status byte
const TX_STAT_DD: u8 = 1 << 0;   // Descriptor Done

// TCTL bits
const TCTL_EN:    u32 = 1 << 1;
const TCTL_PSP:   u32 = 1 << 3;
const TCTL_CT_SHIFT:   u32 = 4;
const TCTL_COLD_SHIFT: u32 = 12;

// RCTL bits
const RCTL_EN:    u32 = 1 << 1;
const RCTL_SBP:   u32 = 1 << 2;
const RCTL_UPE:   u32 = 1 << 3;   // Unicast promiscuous
const RCTL_MPE:   u32 = 1 << 4;   // Multicast promiscuous
const RCTL_BAM:   u32 = 1 << 15;  // Broadcast accept
const RCTL_BSIZE_2048: u32 = 0 << 16;   // 2 KB buffers
const RCTL_SECRC: u32 = 1 << 26;  // Strip Ethernet CRC

// RX descriptor status byte
const RX_STAT_DD:  u8 = 1 << 0;
const RX_STAT_EOP: u8 = 1 << 1;

const NUM_TX: usize = 8;
const NUM_RX: usize = 16;
const RX_BUF_SIZE: usize = 2048;
const TX_BUF_SIZE: usize = 2048;

// ── Descriptor types (16 bytes each, packed) ────────────────────────────────

#[repr(C, packed)]
#[derive(Copy, Clone)]
struct TxDesc {
    addr:    u64,
    length:  u16,
    cso:     u8,
    cmd:     u8,
    status:  u8,
    css:     u8,
    special: u16,
}

#[repr(C, packed)]
#[derive(Copy, Clone)]
struct RxDesc {
    addr:     u64,
    length:   u16,
    checksum: u16,
    status:   u8,
    errors:   u8,
    special:  u16,
}

// ── Public types ─────────────────────────────────────────────────────────────

pub struct NicInfo {
    pub addr:        pci::PciDevice,
    pub mmio_phys:   u64,
    pub mmio_virt:   u64,
    pub mac:         [u8; 6],
    pub link_up:     bool,
    pub speed_mbps:  u32,
    pub full_duplex: bool,
}

/// Initialised e1000 driver — owns the TX/RX rings and buffers.
/// Heap allocations are kept alive in the Vec fields.
pub struct Driver {
    pub nic:        NicInfo,
    hhdm:           u64,
    tx_ring:        Vec<TxDesc>,
    tx_bufs:        Vec<u8>,        // NUM_TX × TX_BUF_SIZE flat buffer
    tx_idx:         usize,          // next slot to enqueue
    rx_ring:        Vec<RxDesc>,
    rx_bufs:        Vec<u8>,        // NUM_RX × RX_BUF_SIZE flat buffer
    rx_idx:         usize,          // next slot we'll inspect
    pub tx_sent:    u32,
    pub rx_recv:    u32,
}

#[inline(always)]
unsafe fn mmio_r32(addr: u64) -> u32 { core::ptr::read_volatile(addr as *const u32) }
#[inline(always)]
unsafe fn mmio_w32(addr: u64, v: u32) { core::ptr::write_volatile(addr as *mut u32, v) }

/// Find + identify the first Intel e1000-class NIC.
pub fn probe(hhdm_offset: u64) -> Option<NicInfo> {
    if hhdm_offset == 0 { return None; }
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

    let raw0 = pci::pci_read32(nic.bus, nic.dev, nic.func, 0x10);
    if raw0 & 1 != 0 { return None; }
    let bar_type = (raw0 >> 1) & 3;
    let base_phys: u64 = if bar_type == 2 {
        nic.read_bar64(0x10)
    } else {
        (raw0 & 0xFFFF_FFF0) as u64
    };
    if base_phys == 0 || base_phys >= 0x0000_8000_0000_0000 { return None; }
    let base = base_phys.wrapping_add(hhdm_offset);

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
        mac, link_up, speed_mbps, full_duplex,
    })
}

pub unsafe fn soft_reset(nic: &NicInfo) {
    let ctrl = mmio_r32(nic.mmio_virt + REG_CTRL);
    mmio_w32(nic.mmio_virt + REG_CTRL, ctrl | CTRL_RST);
    for _ in 0..1024 { let _ = mmio_r32(nic.mmio_virt + REG_STATUS); }
    mmio_w32(nic.mmio_virt + REG_IMC, 0xFFFF_FFFF);
    let _ = mmio_r32(nic.mmio_virt + REG_ICR);
    let ctrl = mmio_r32(nic.mmio_virt + REG_CTRL);
    mmio_w32(nic.mmio_virt + REG_CTRL, ctrl | CTRL_SLU | CTRL_FD);
}

// ── Driver init / send / receive ────────────────────────────────────────────

impl Driver {
    /// Translate a virtual address (in Limine's HHDM region) back to its
    /// physical address.  Used to give the NIC's DMA engine pointers it
    /// can resolve in physical RAM.
    #[inline(always)]
    fn phys(&self, virt: *const u8) -> u64 {
        (virt as u64).wrapping_sub(self.hhdm)
    }

    pub fn init(nic: NicInfo, hhdm: u64) -> Self {
        let mut tx_ring: Vec<TxDesc> = alloc::vec![TxDesc{addr:0,length:0,cso:0,cmd:0,status:0,css:0,special:0}; NUM_TX];
        let     tx_bufs: Vec<u8>     = alloc::vec![0u8; NUM_TX * TX_BUF_SIZE];

        let mut rx_ring: Vec<RxDesc> = alloc::vec![RxDesc{addr:0,length:0,checksum:0,status:0,errors:0,special:0}; NUM_RX];
        let     rx_bufs: Vec<u8>     = alloc::vec![0u8; NUM_RX * RX_BUF_SIZE];

        // Fill RX descriptors with the physical addresses of our buffers so
        // the NIC knows where to DMA-write incoming frames.
        for i in 0..NUM_RX {
            let buf_virt = unsafe { rx_bufs.as_ptr().add(i * RX_BUF_SIZE) };
            let buf_phys = (buf_virt as u64).wrapping_sub(hhdm);
            rx_ring[i].addr = buf_phys;
        }

        let mut drv = Self {
            nic, hhdm,
            tx_ring, tx_bufs, tx_idx: 0,
            rx_ring, rx_bufs, rx_idx: 0,
            tx_sent: 0, rx_recv: 0,
        };

        unsafe { drv.configure(); }
        drv
    }

    unsafe fn configure(&mut self) {
        let base = self.nic.mmio_virt;

        // Clear multicast filter table (16 dwords at 0x5200).
        for i in 0..128u64 {
            mmio_w32(base + 0x5200 + i * 4, 0);
        }

        // ── TX ring ──────────────────────────────────────────────────────
        let tx_phys = (self.tx_ring.as_ptr() as u64).wrapping_sub(self.hhdm);
        mmio_w32(base + REG_TDBAL, (tx_phys & 0xFFFF_FFFF) as u32);
        mmio_w32(base + REG_TDBAH, (tx_phys >> 32)        as u32);
        mmio_w32(base + REG_TDLEN, (NUM_TX * 16) as u32);
        mmio_w32(base + REG_TDH, 0);
        mmio_w32(base + REG_TDT, 0);
        // TCTL: enable + pad short + collision threshold 0x10 + 64-byte gap
        mmio_w32(base + REG_TCTL,
                 TCTL_EN | TCTL_PSP
                 | (0x10 << TCTL_CT_SHIFT)
                 | (0x40 << TCTL_COLD_SHIFT));
        mmio_w32(base + REG_TIPG, 0x0060_200A);   // IEEE 802.3 standard gap

        // ── RX ring ──────────────────────────────────────────────────────
        let rx_phys = (self.rx_ring.as_ptr() as u64).wrapping_sub(self.hhdm);
        mmio_w32(base + REG_RDBAL, (rx_phys & 0xFFFF_FFFF) as u32);
        mmio_w32(base + REG_RDBAH, (rx_phys >> 32)        as u32);
        mmio_w32(base + REG_RDLEN, (NUM_RX * 16) as u32);
        mmio_w32(base + REG_RDH, 0);
        mmio_w32(base + REG_RDT, (NUM_RX - 1) as u32);   // tail points to last
        // RCTL: enable + broadcast accept + 2 KB buffers + strip CRC
        // (don't promiscuous yet — only unicast-to-us + broadcast)
        mmio_w32(base + REG_RCTL,
                 RCTL_EN | RCTL_BAM | RCTL_BSIZE_2048 | RCTL_SECRC
                 // Make sure we see SBP+UPE+MPE so we don't drop anything
                 // while bringing up the network — easier to debug.
                 | RCTL_SBP | RCTL_UPE | RCTL_MPE);
    }

    /// Enqueue a raw Ethernet frame for transmission.  Returns Err if the
    /// ring is full or the frame is too large.
    pub fn send(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        if frame.len() > TX_BUF_SIZE { return Err("frame too large"); }
        if frame.len() < 60 { /* will be padded by NIC because TCTL.PSP */ }

        let idx = self.tx_idx;
        // Copy frame into our TX buffer slot
        let buf_off = idx * TX_BUF_SIZE;
        let dst = &mut self.tx_bufs[buf_off..buf_off + frame.len()];
        dst.copy_from_slice(frame);

        let buf_virt = unsafe { self.tx_bufs.as_ptr().add(buf_off) };
        let buf_phys = self.phys(buf_virt);

        self.tx_ring[idx].addr   = buf_phys;
        self.tx_ring[idx].length = frame.len() as u16;
        self.tx_ring[idx].cmd    = TX_CMD_EOP | TX_CMD_IFCS | TX_CMD_RS;
        self.tx_ring[idx].status = 0;

        // Advance tail
        self.tx_idx = (idx + 1) % NUM_TX;
        unsafe {
            mmio_w32(self.nic.mmio_virt + REG_TDT, self.tx_idx as u32);
        }

        // Wait for status.DD (descriptor done).  Bounded spin.
        let mut spins = 0u32;
        while self.tx_ring[idx].status & TX_STAT_DD == 0 {
            spins += 1;
            if spins > 1_000_000 { return Err("TX timeout"); }
            unsafe { core::arch::asm!("pause"); }
        }
        self.tx_sent += 1;
        Ok(())
    }

    /// Build a broadcast ARP request asking "who has `target_ip`?" and send it.
    pub fn send_arp(&mut self, sender_ip: [u8; 4], target_ip: [u8; 4]) -> Result<(), &'static str> {
        let mut frame = [0u8; 42];

        // Ethernet header
        frame[0..6].copy_from_slice(&[0xFF; 6]);          // dst = broadcast
        frame[6..12].copy_from_slice(&self.nic.mac);      // src
        frame[12..14].copy_from_slice(&0x0806u16.to_be_bytes()); // ARP ethertype

        // ARP payload
        frame[14..16].copy_from_slice(&1u16.to_be_bytes());     // htype = Ethernet
        frame[16..18].copy_from_slice(&0x0800u16.to_be_bytes());// ptype = IPv4
        frame[18] = 6;                                          // hlen
        frame[19] = 4;                                          // plen
        frame[20..22].copy_from_slice(&1u16.to_be_bytes());     // op = request
        frame[22..28].copy_from_slice(&self.nic.mac);           // sender MAC
        frame[28..32].copy_from_slice(&sender_ip);              // sender IP
        frame[32..38].copy_from_slice(&[0u8; 6]);               // target MAC (unknown)
        frame[38..42].copy_from_slice(&target_ip);              // target IP

        self.send(&frame)
    }

    /// Drain one ready RX descriptor if any.  Returns Some((bytes_used,
    /// status)) on success — caller can then `rx_frame(idx)` to read the
    /// bytes.  Each call advances `rx_idx` and re-arms the slot.
    pub fn poll_rx(&mut self) -> Option<(usize, usize)> {
        let idx = self.rx_idx;
        if self.rx_ring[idx].status & RX_STAT_DD == 0 {
            return None;
        }
        let len = self.rx_ring[idx].length as usize;
        // Clear status + bump RDT so the slot is reusable.
        self.rx_ring[idx].status = 0;
        let new_tail = idx;
        self.rx_idx = (idx + 1) % NUM_RX;
        unsafe {
            mmio_w32(self.nic.mmio_virt + REG_RDT, new_tail as u32);
        }
        self.rx_recv += 1;
        Some((idx, len))
    }

    pub fn rx_frame(&self, idx: usize) -> &[u8] {
        let off = idx * RX_BUF_SIZE;
        let len = self.rx_ring[idx].length as usize;
        &self.rx_bufs[off..off + len.min(RX_BUF_SIZE)]
    }
}
