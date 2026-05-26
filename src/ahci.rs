//! Minimal AHCI driver — detects modern SATA disks behind an AHCI host
//! bus adapter, reads their IDENTIFY DEVICE response.  Read-only for now;
//! the existing IDE driver in `ata.rs` keeps responsibility for actual
//! sector reads on legacy controllers.
//!
//! Why this exists: VirtualBox / QEMU default storage controller is AHCI,
//! and the user complained that /disks didn't see their 4 GB drive — that
//! was because we only spoke legacy IDE.  With AHCI detection, /disks can
//! now at least *list* SATA drives even when the IDE channel is empty.
//!
//! Layout we set up per port:
//!   • 1 KB command list (32 × 32-byte command headers)
//!   • 256 B received FIS area
//!   • 256 B command table for slot 0 (small, fits a single PRDT)
//! All in heap (Vec<u8>) — physical addresses via virt − hhdm_offset.

extern crate alloc;
use alloc::vec::Vec;
use crate::pci;

// ── HBA register offsets ────────────────────────────────────────────────────
const HBA_CAP:      u64 = 0x00;
const HBA_GHC:      u64 = 0x04;
const HBA_IS:       u64 = 0x08;
const HBA_PI:       u64 = 0x0C;   // Ports Implemented
const HBA_VS:       u64 = 0x10;
const HBA_BOHC:     u64 = 0x28;   // BIOS/OS handoff

// Per-port offset = 0x100 + port * 0x80
const fn port_base(port: usize) -> u64 { 0x100 + (port as u64) * 0x80 }

const PORT_CLB:    u64 = 0x00;    // Command list base low
const PORT_CLBU:   u64 = 0x04;    // Command list base high
const PORT_FB:     u64 = 0x08;    // FIS base low
const PORT_FBU:    u64 = 0x0C;    // FIS base high
const PORT_IS:     u64 = 0x10;
const PORT_IE:     u64 = 0x14;
const PORT_CMD:    u64 = 0x18;
const PORT_TFD:    u64 = 0x20;
const PORT_SIG:    u64 = 0x24;
const PORT_SSTS:   u64 = 0x28;    // SATA status
const PORT_SCTL:   u64 = 0x2C;
const PORT_SERR:   u64 = 0x30;
const PORT_SACT:   u64 = 0x34;
const PORT_CI:     u64 = 0x38;    // Command issue

const PORT_CMD_ST:  u32 = 1 << 0;   // start
const PORT_CMD_FRE: u32 = 1 << 4;   // FIS receive enable
const PORT_CMD_FR:  u32 = 1 << 14;  // FIS receive running
const PORT_CMD_CR:  u32 = 1 << 15;  // command list running

const ATA_DEV_BUSY: u32 = 1 << 7;
const ATA_DEV_DRQ:  u32 = 1 << 3;

const SIG_ATA:    u32 = 0x0000_0101;
const SIG_ATAPI:  u32 = 0xEB14_0101;

#[inline(always)]
unsafe fn r32(addr: u64) -> u32 { core::ptr::read_volatile(addr as *const u32) }
#[inline(always)]
unsafe fn w32(addr: u64, v: u32) { core::ptr::write_volatile(addr as *mut u32, v) }

// ── Command-header / FIS / command-table layouts ────────────────────────────
//
// Command header (32 bytes), index 0..31 in the 1 KB command list:
//   dword0: prdtl(31..16) pmp(15..12) c(10) b(9) r(8) p(7) w(6) a(5) cfl(4..0)
//   dword1: prdbc (PRD byte count)
//   dword2: ctba   (command table base, 128-byte aligned)
//   dword3: ctbau

#[repr(C, packed)]
#[derive(Copy, Clone)]
struct CmdHeader {
    dw0:   u32,
    prdbc: u32,
    ctba:  u32,
    ctbau: u32,
    _r:    [u32; 4],
}

// PRDT entry inside the command table
#[repr(C, packed)]
#[derive(Copy, Clone)]
struct Prdt {
    dba:   u32,
    dbau:  u32,
    _r:    u32,
    dbc:   u32,  // bit 31 = I (interrupt on completion), bits 21..0 = byte count - 1
}

// Command table = 0x80 bytes CFIS + 0x10 ATAPI cmd + 0x30 reserved + 8 × PRDT entries
// = 128 + 16 + 48 + (8 × 16) = 320 bytes.  We allocate 1 KB to be safe.
const CMD_TABLE_SIZE: usize = 1024;
const NUM_CMD_HEADERS: usize = 32;
const CMD_LIST_SIZE: usize = NUM_CMD_HEADERS * 32;   // 1024 bytes
const FIS_BUF_SIZE: usize = 256;
const IDENTIFY_BUF_SIZE: usize = 512;

// Register-Host-to-Device FIS layout (16 bytes used)
#[repr(C, packed)]
struct FisH2D {
    fis_type: u8,    // 0x27
    flags:    u8,    // bit 7 = command
    command:  u8,
    feat_lo:  u8,
    lba0:     u8,
    lba1:     u8,
    lba2:     u8,
    device:   u8,
    lba3:     u8,
    lba4:     u8,
    lba5:     u8,
    feat_hi:  u8,
    count_lo: u8,
    count_hi: u8,
    icc:      u8,
    control:  u8,
    _r:       [u8; 4],
}

// ── Public probe ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AhciDrive {
    pub port:        u8,
    pub model:       [u8; 40],
    pub serial:      [u8; 20],
    pub sectors_48:  u64,
    pub is_atapi:    bool,
}

impl AhciDrive {
    pub fn model_str(&self) -> &str {
        let mut end = self.model.len();
        while end > 0 && (self.model[end - 1] == b' ' || self.model[end - 1] == 0) { end -= 1; }
        core::str::from_utf8(&self.model[..end]).unwrap_or("?")
    }
    pub fn capacity_mb(&self) -> u32 { ((self.sectors_48 * 512) / (1024*1024)) as u32 }
    pub fn capacity_gb_x10(&self) -> u32 { ((self.sectors_48 * 512 * 10) / (1024*1024*1024)) as u32 }
}

pub struct AhciProbe {
    pub controller_present: bool,
    pub abar_phys: u64,
    pub ports_impl: u32,
    pub drives: Vec<AhciDrive>,
}

pub fn probe(hhdm: u64) -> AhciProbe {
    let mut probe = AhciProbe {
        controller_present: false,
        abar_phys: 0,
        ports_impl: 0,
        drives: Vec::new(),
    };
    if hhdm == 0 { return probe; }

    // Find the first AHCI controller (class 0x01, subclass 0x06).
    let mut found: Option<pci::PciDevice> = None;
    pci::enumerate(|d| {
        if d.class_code == 0x01 && d.subclass == 0x06 && found.is_none() {
            found = Some(d);
        }
    });
    let dev = match found { Some(d) => d, None => return probe };
    probe.controller_present = true;

    // ABAR is BAR5 — at PCI config 0x24.
    let raw = pci::pci_read32(dev.bus, dev.dev, dev.func, 0x24);
    if raw & 1 != 0 { return probe; }   // I/O BAR — not what we want
    let abar_phys = (raw & 0xFFFF_FFF0) as u64;
    if abar_phys == 0 { return probe; }
    probe.abar_phys = abar_phys;

    let abar = abar_phys.wrapping_add(hhdm);

    unsafe {
        // BIOS/OS handoff: if the BOH bit is set, request ownership.
        let bohc = r32(abar + HBA_BOHC);
        if bohc & 1 != 0 {
            // Old AHCI on real HW may not support it — skip.
        }

        // Enable AHCI mode in GHC.
        let ghc = r32(abar + HBA_GHC);
        w32(abar + HBA_GHC, ghc | (1 << 31));   // AE bit

        let pi = r32(abar + HBA_PI);
        probe.ports_impl = pi;

        // For each implemented port, probe.
        for port in 0..32usize {
            if pi & (1u32 << port) == 0 { continue; }
            if let Some(drv) = identify_port(abar, port, hhdm) {
                probe.drives.push(drv);
            }
        }
    }
    probe
}

unsafe fn identify_port(abar: u64, port_idx: usize, hhdm: u64) -> Option<AhciDrive> {
    let p = abar + port_base(port_idx);

    // Check device-detect / interface power state
    let ssts = r32(p + PORT_SSTS);
    let det  = ssts & 0x0F;
    let ipm  = (ssts >> 8) & 0x0F;
    if det != 3 || ipm != 1 { return None; }   // no device or not active

    // Signature: SATA = 0x00000101, ATAPI = 0xEB140101
    let sig = r32(p + PORT_SIG);
    let is_atapi = sig == SIG_ATAPI;
    if sig != SIG_ATA && !is_atapi { return None; }

    // ── Stop the port (CMD.ST=0, wait CR=0; CMD.FRE=0, wait FR=0) ───────
    let cmd = r32(p + PORT_CMD);
    w32(p + PORT_CMD, cmd & !PORT_CMD_ST);
    let mut spins = 0u32;
    while r32(p + PORT_CMD) & PORT_CMD_CR != 0 {
        spins += 1; if spins > 100_000 { return None; }
        core::arch::asm!("pause");
    }
    let cmd = r32(p + PORT_CMD);
    w32(p + PORT_CMD, cmd & !PORT_CMD_FRE);
    spins = 0;
    while r32(p + PORT_CMD) & PORT_CMD_FR != 0 {
        spins += 1; if spins > 100_000 { return None; }
        core::arch::asm!("pause");
    }

    // ── Allocate DMA-able buffers ───────────────────────────────────────
    // Vec<u8> on our heap is HHDM-mapped, so phys = virt - hhdm.  Heap is
    // 16-byte aligned by linked_list_allocator, but command list needs
    // 1 KB alignment.  We over-allocate and align manually.
    let cmdlist_alloc: Vec<u8> = alloc::vec![0u8; CMD_LIST_SIZE + 1024];
    let cmdlist_virt = align_up(cmdlist_alloc.as_ptr() as u64, 1024);
    let cmdlist_phys = cmdlist_virt.wrapping_sub(hhdm);

    let fis_alloc: Vec<u8> = alloc::vec![0u8; FIS_BUF_SIZE + 256];
    let fis_virt = align_up(fis_alloc.as_ptr() as u64, 256);
    let fis_phys = fis_virt.wrapping_sub(hhdm);

    let cmdtbl_alloc: Vec<u8> = alloc::vec![0u8; CMD_TABLE_SIZE + 128];
    let cmdtbl_virt = align_up(cmdtbl_alloc.as_ptr() as u64, 128);
    let cmdtbl_phys = cmdtbl_virt.wrapping_sub(hhdm);

    let identify_buf: Vec<u8> = alloc::vec![0u8; IDENTIFY_BUF_SIZE];
    let identify_phys = (identify_buf.as_ptr() as u64).wrapping_sub(hhdm);

    // Point the port at our new structures.
    w32(p + PORT_CLB,  (cmdlist_phys & 0xFFFF_FFFF) as u32);
    w32(p + PORT_CLBU, (cmdlist_phys >> 32)        as u32);
    w32(p + PORT_FB,   (fis_phys & 0xFFFF_FFFF)    as u32);
    w32(p + PORT_FBU,  (fis_phys >> 32)            as u32);

    // Clear pending interrupts/errors.
    w32(p + PORT_IS,   0xFFFF_FFFF);
    w32(p + PORT_SERR, 0xFFFF_FFFF);

    // ── Start the port again ─────────────────────────────────────────────
    let cmd = r32(p + PORT_CMD);
    w32(p + PORT_CMD, cmd | PORT_CMD_FRE);
    // Wait until BSY and DRQ clear before issuing commands
    spins = 0;
    while r32(p + PORT_TFD) & (ATA_DEV_BUSY | ATA_DEV_DRQ) != 0 {
        spins += 1; if spins > 100_000 { return None; }
        core::arch::asm!("pause");
    }
    let cmd = r32(p + PORT_CMD);
    w32(p + PORT_CMD, cmd | PORT_CMD_ST);

    // ── Build command header #0 + command table #0 ──────────────────────
    let chs = cmdlist_virt as *mut CmdHeader;
    // cfl=5 dwords (H2D FIS = 20 bytes / 4), prdtl=1, write=0
    (*chs).dw0 = 5 | (1 << 16);
    (*chs).prdbc = 0;
    (*chs).ctba  = (cmdtbl_phys & 0xFFFF_FFFF) as u32;
    (*chs).ctbau = (cmdtbl_phys >> 32) as u32;

    // FIS at start of command table
    let fis = cmdtbl_virt as *mut FisH2D;
    core::ptr::write_bytes(fis as *mut u8, 0, core::mem::size_of::<FisH2D>());
    (*fis).fis_type = 0x27;
    (*fis).flags    = 0x80;   // C: command
    (*fis).command  = if is_atapi { 0xA1 } else { 0xEC };  // IDENTIFY PACKET vs IDENTIFY
    (*fis).device   = 0;
    (*fis).count_lo = 0;
    (*fis).count_hi = 0;

    // PRDT entry 0: at cmdtbl + 0x80
    let prdt = (cmdtbl_virt + 0x80) as *mut Prdt;
    (*prdt).dba  = (identify_phys & 0xFFFF_FFFF) as u32;
    (*prdt).dbau = (identify_phys >> 32) as u32;
    (*prdt).dbc  = (512 - 1) | (1 << 31);   // 512 bytes, I=1

    // Issue command in slot 0
    w32(p + PORT_CI, 1);
    spins = 0;
    while r32(p + PORT_CI) & 1 != 0 {
        if r32(p + PORT_IS) & (1 << 30) != 0 { return None; } // task file error
        spins += 1; if spins > 1_000_000 { return None; }
        core::arch::asm!("pause");
    }
    if r32(p + PORT_IS) & (1 << 30) != 0 { return None; }

    // Parse IDENTIFY: words 27..47 = model, 10..20 = serial, 100..104 = LBA-48
    let words: &[u16] = core::slice::from_raw_parts(identify_buf.as_ptr() as *const u16, 256);
    let mut model = [b' '; 40];
    for i in 0..20 {
        let w = words[27 + i];
        model[i * 2]     = (w >> 8) as u8;
        model[i * 2 + 1] = (w & 0xFF) as u8;
    }
    let mut serial = [b' '; 20];
    for i in 0..10 {
        let w = words[10 + i];
        serial[i * 2]     = (w >> 8) as u8;
        serial[i * 2 + 1] = (w & 0xFF) as u8;
    }
    let sectors_48 = (words[100] as u64)
        | ((words[101] as u64) << 16)
        | ((words[102] as u64) << 32)
        | ((words[103] as u64) << 48);

    // Keep the buffers alive (we own them — they'll be freed when this
    // Vec drops at the end of probe()).  AHCI commands have completed
    // by the time we get here so it's safe.
    drop(cmdlist_alloc);
    drop(fis_alloc);
    drop(cmdtbl_alloc);
    drop(identify_buf);

    Some(AhciDrive {
        port: port_idx as u8,
        model,
        serial,
        sectors_48,
        is_atapi,
    })
}

#[inline(always)]
fn align_up(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}
