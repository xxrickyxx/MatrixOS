//! ATA / IDE PIO driver.
//!
//! Bare-metal PIO mode for the legacy 0x1F0 (primary) and 0x170 (secondary)
//! controllers.  Each controller has master + slave drives.  We:
//!
//!   • detect drives by sending IDENTIFY (0xEC)
//!   • parse model string + serial + sector count from the IDENTIFY reply
//!   • expose `read_sectors(drive, lba, count, buf)` (PIO 28-bit LBA)
//!   • expose `write_sectors(...)` — present but DANGEROUS — kept gated
//!     behind an `unsafe` boolean flag in /install so we never write by
//!     accident.
//!
//! Limitations: PIO only (slow), 28-bit LBA (max 128 GiB), no DMA, single-
//! thread (we have no scheduler anyway).  Good enough for read-only probe
//! and the eventual ISO-to-disk one-shot install.

use core::arch::asm;

// ── Port layout ──────────────────────────────────────────────────────────────

pub const PRIMARY_IO:   u16 = 0x1F0;
pub const PRIMARY_CTL:  u16 = 0x3F6;
pub const SECONDARY_IO: u16 = 0x170;
pub const SECONDARY_CTL: u16 = 0x376;

// Register offsets from the IO base.
const REG_DATA:     u16 = 0;
const REG_ERROR:    u16 = 1;   // read
const REG_FEATURES: u16 = 1;   // write
const REG_SECCOUNT: u16 = 2;
const REG_LBA_LO:   u16 = 3;
const REG_LBA_MID:  u16 = 4;
const REG_LBA_HI:   u16 = 5;
const REG_DRIVE:    u16 = 6;
const REG_STATUS:   u16 = 7;   // read
const REG_COMMAND:  u16 = 7;   // write

// Status flags
const ST_ERR:  u8 = 0x01;
const ST_DRQ:  u8 = 0x08;
const ST_DF:   u8 = 0x20;
const ST_RDY:  u8 = 0x40;
const ST_BSY:  u8 = 0x80;

// Commands
const CMD_READ_PIO:   u8 = 0x20;
const CMD_WRITE_PIO:  u8 = 0x30;
const CMD_CACHE_FLUSH: u8 = 0xE7;
const CMD_IDENTIFY:    u8 = 0xEC;
const CMD_IDENTIFY_PACKET: u8 = 0xA1;

// ── Low-level port I/O ───────────────────────────────────────────────────────

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
#[inline(always)]
unsafe fn inw(port: u16) -> u16 {
    let v: u16;
    asm!("in ax, dx", in("dx") port, out("ax") v, options(nomem, nostack, preserves_flags));
    v
}
#[inline(always)]
unsafe fn outw(port: u16, val: u16) {
    asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack, preserves_flags));
}

/// 400ns busy-wait by reading the alternate status register four times.
unsafe fn io_wait(io: u16) {
    let _ = inb(io + REG_STATUS);
    let _ = inb(io + REG_STATUS);
    let _ = inb(io + REG_STATUS);
    let _ = inb(io + REG_STATUS);
}

/// Wait until BSY clears and DRQ is set (or ERR/DF tripped).
unsafe fn wait_drq(io: u16) -> Result<(), &'static str> {
    let mut timeout = 100_000u32;
    loop {
        let s = inb(io + REG_STATUS);
        if s & ST_ERR != 0 { return Err("ATA: ERR"); }
        if s & ST_DF  != 0 { return Err("ATA: device fault"); }
        if s & ST_BSY == 0 && s & ST_DRQ != 0 { return Ok(()); }
        timeout -= 1;
        if timeout == 0 { return Err("ATA: timeout"); }
        for _ in 0..16 { asm!("pause"); }
    }
}

/// Wait until BSY clears and RDY is set, no DRQ required (used after writes).
unsafe fn wait_ready(io: u16) -> Result<(), &'static str> {
    let mut timeout = 100_000u32;
    loop {
        let s = inb(io + REG_STATUS);
        if s & ST_ERR != 0 { return Err("ATA: ERR"); }
        if s & ST_BSY == 0 && s & ST_RDY != 0 { return Ok(()); }
        timeout -= 1;
        if timeout == 0 { return Err("ATA: timeout"); }
        for _ in 0..16 { asm!("pause"); }
    }
}

// ── Drive identity ───────────────────────────────────────────────────────────

#[derive(Copy, Clone)]
pub enum Channel { Primary, Secondary }
#[derive(Copy, Clone)]
pub enum Position { Master, Slave }

#[derive(Copy, Clone)]
pub struct DriveAddr {
    pub channel:  Channel,
    pub position: Position,
}

impl DriveAddr {
    pub fn io_base(&self) -> u16 {
        match self.channel { Channel::Primary => PRIMARY_IO, Channel::Secondary => SECONDARY_IO }
    }
    pub fn ctl_base(&self) -> u16 {
        match self.channel { Channel::Primary => PRIMARY_CTL, Channel::Secondary => SECONDARY_CTL }
    }
    pub fn slave_bit(&self) -> u8 {
        match self.position { Position::Master => 0, Position::Slave => 0x10 }
    }
    pub fn label(&self) -> &'static str {
        match (self.channel, self.position) {
            (Channel::Primary,   Position::Master) => "P-Master",
            (Channel::Primary,   Position::Slave)  => "P-Slave",
            (Channel::Secondary, Position::Master) => "S-Master",
            (Channel::Secondary, Position::Slave)  => "S-Slave",
        }
    }
}

#[derive(Clone)]
pub struct DriveInfo {
    pub addr:        DriveAddr,
    pub model:       [u8; 40],
    pub serial:      [u8; 20],
    pub sectors_28:  u32,    // 28-bit LBA total (capped 128 GiB)
    pub sectors_48:  u64,    // 48-bit LBA total (real capacity if supported)
    pub is_atapi:    bool,
}

impl DriveInfo {
    /// Total sectors — picks LBA-48 when present, otherwise LBA-28.
    /// Many virtual disks (VirtualBox, QEMU) report only LBA-28 even for >128 GB
    /// drives, so this is the value to trust for capacity.
    pub fn total_sectors(&self) -> u64 {
        if self.sectors_48 > 0 { self.sectors_48 } else { self.sectors_28 as u64 }
    }
    pub fn capacity_mb(&self) -> u32 {
        ((self.total_sectors() * 512) / (1024 * 1024)) as u32
    }
    pub fn capacity_gb_x10(&self) -> u32 {
        // GB × 10 so the caller can format "X.Y GB" without floats
        ((self.total_sectors() * 512 * 10) / (1024 * 1024 * 1024)) as u32
    }
    pub fn model_str(&self) -> &str {
        // Find end of trailing whitespace
        let mut end = self.model.len();
        while end > 0 && (self.model[end - 1] == b' ' || self.model[end - 1] == 0) { end -= 1; }
        core::str::from_utf8(&self.model[..end]).unwrap_or("?")
    }
    pub fn serial_str(&self) -> &str {
        let mut end = self.serial.len();
        while end > 0 && (self.serial[end - 1] == b' ' || self.serial[end - 1] == 0) { end -= 1; }
        core::str::from_utf8(&self.serial[..end]).unwrap_or("?")
    }
}

/// Send IDENTIFY (or IDENTIFY_PACKET) to a drive and parse its response.
/// Returns None if no drive is present.
pub fn identify(addr: DriveAddr) -> Option<DriveInfo> {
    let io = addr.io_base();
    unsafe {
        // Select drive.
        outb(io + REG_DRIVE, 0xA0 | addr.slave_bit());
        io_wait(io);

        // Clear sector count + LBA registers.
        outb(io + REG_SECCOUNT, 0);
        outb(io + REG_LBA_LO,   0);
        outb(io + REG_LBA_MID,  0);
        outb(io + REG_LBA_HI,   0);

        // Send IDENTIFY.
        outb(io + REG_COMMAND, CMD_IDENTIFY);
        io_wait(io);

        let status = inb(io + REG_STATUS);
        if status == 0 { return None; }   // no drive on bus

        // Wait for BSY to clear.
        let mut timeout = 100_000u32;
        loop {
            let s = inb(io + REG_STATUS);
            if s & ST_BSY == 0 { break; }
            timeout -= 1;
            if timeout == 0 { return None; }
            for _ in 0..16 { asm!("pause"); }
        }

        // ATAPI / SATA signature check: LBA_MID/LBA_HI become non-zero.
        let mid = inb(io + REG_LBA_MID);
        let hi  = inb(io + REG_LBA_HI);
        let is_atapi = (mid == 0x14 && hi == 0xEB) || (mid == 0x69 && hi == 0x96);
        if mid != 0 || hi != 0 {
            // Drive exists but isn't standard ATA.  Try IDENTIFY_PACKET for
            // ATAPI; if neither works, skip.
            if is_atapi {
                outb(io + REG_COMMAND, CMD_IDENTIFY_PACKET);
                io_wait(io);
            } else {
                return None;
            }
        }

        // Now wait for DRQ.
        let s = inb(io + REG_STATUS);
        if s & ST_ERR != 0 { return None; }
        if wait_drq(io).is_err() { return None; }

        // Read 256 16-bit words.
        let mut words = [0u16; 256];
        for w in words.iter_mut() {
            *w = inw(io + REG_DATA);
        }

        // Parse:
        //   words 27..47 = model (ATA strings are byte-swapped pairs)
        //   words 10..20 = serial
        //   words 60..62 = total sectors (28-bit LBA)
        //   words 100..104 = total sectors (48-bit LBA), if supported
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
        let sectors_28 = (words[60] as u32) | ((words[61] as u32) << 16);
        let sectors_48 = (words[100] as u64)
            | ((words[101] as u64) << 16)
            | ((words[102] as u64) << 32)
            | ((words[103] as u64) << 48);

        Some(DriveInfo {
            addr,
            model,
            serial,
            sectors_28,
            sectors_48,
            is_atapi,
        })
    }
}

// ── Probe all 4 drive slots ──────────────────────────────────────────────────

pub struct Probe {
    pub drives: [Option<DriveInfo>; 4],
}

impl Probe {
    pub fn count(&self) -> usize { self.drives.iter().filter(|d| d.is_some()).count() }
}

pub fn probe_all() -> Probe {
    let addrs = [
        DriveAddr { channel: Channel::Primary,   position: Position::Master },
        DriveAddr { channel: Channel::Primary,   position: Position::Slave  },
        DriveAddr { channel: Channel::Secondary, position: Position::Master },
        DriveAddr { channel: Channel::Secondary, position: Position::Slave  },
    ];
    let mut drives: [Option<DriveInfo>; 4] = [const { None }; 4];
    for (i, a) in addrs.iter().enumerate() {
        drives[i] = identify(*a);
    }
    Probe { drives }
}

// ── Sector read (28-bit LBA) ─────────────────────────────────────────────────

pub fn read_sectors(addr: DriveAddr, lba: u32, count: u8, buf: &mut [u8]) -> Result<(), &'static str> {
    if buf.len() < count as usize * 512 {
        return Err("buffer too small");
    }
    let io = addr.io_base();
    unsafe {
        outb(io + REG_DRIVE, 0xE0 | addr.slave_bit() | ((lba >> 24) & 0x0F) as u8);
        io_wait(io);
        outb(io + REG_SECCOUNT, count);
        outb(io + REG_LBA_LO,  (lba        & 0xFF) as u8);
        outb(io + REG_LBA_MID, ((lba >> 8)  & 0xFF) as u8);
        outb(io + REG_LBA_HI,  ((lba >> 16) & 0xFF) as u8);
        outb(io + REG_COMMAND, CMD_READ_PIO);

        for s in 0..count as usize {
            wait_drq(io)?;
            let base = s * 512;
            for w in 0..256 {
                let v = inw(io + REG_DATA);
                buf[base + w * 2]     = (v & 0xFF) as u8;
                buf[base + w * 2 + 1] = (v >> 8) as u8;
            }
            io_wait(io);
        }
    }
    Ok(())
}

// ── Sector write (28-bit LBA) — DANGEROUS, only called from /install ────────

// ── Bulk install: write a whole byte slice to disk starting at `lba_start` ──
//
// Chunked into 64-sector PIO writes so 3 GB fits in ~12 000 commands instead
// of 6 M.  Calls `progress(bytes_written, total_bytes)` periodically so the
// caller can paint a progress bar.

pub fn install_image<F: FnMut(usize, usize)>(
    addr:      DriveAddr,
    lba_start: u32,
    data:      &[u8],
    mut progress: F,
) -> Result<u32, &'static str> {
    const CHUNK_SECTORS: u8 = 64;
    const CHUNK_BYTES:   usize = (CHUNK_SECTORS as usize) * 512;
    let total = data.len();
    let mut written = 0usize;
    let mut lba = lba_start;

    let mut tmp = [0u8; CHUNK_BYTES];

    while written < total {
        let n = (total - written).min(CHUNK_BYTES);
        tmp[..n].copy_from_slice(&data[written..written + n]);
        // Zero-pad the trailing partial sector so we don't write stale data.
        if n < CHUNK_BYTES {
            for b in &mut tmp[n..] { *b = 0; }
        }
        let sectors = (n + 511) / 512;
        write_sectors(addr, lba, sectors as u8, &tmp[..sectors * 512])?;
        lba     += sectors as u32;
        written += n;
        progress(written, total);
    }
    Ok(lba - lba_start)
}

pub fn write_sectors(addr: DriveAddr, lba: u32, count: u8, buf: &[u8]) -> Result<(), &'static str> {
    if buf.len() < count as usize * 512 {
        return Err("buffer too small");
    }
    let io = addr.io_base();
    unsafe {
        outb(io + REG_DRIVE, 0xE0 | addr.slave_bit() | ((lba >> 24) & 0x0F) as u8);
        io_wait(io);
        outb(io + REG_SECCOUNT, count);
        outb(io + REG_LBA_LO,  (lba        & 0xFF) as u8);
        outb(io + REG_LBA_MID, ((lba >> 8)  & 0xFF) as u8);
        outb(io + REG_LBA_HI,  ((lba >> 16) & 0xFF) as u8);
        outb(io + REG_COMMAND, CMD_WRITE_PIO);

        for s in 0..count as usize {
            wait_drq(io)?;
            let base = s * 512;
            for w in 0..256 {
                let lo = buf[base + w * 2]     as u16;
                let hi = buf[base + w * 2 + 1] as u16;
                outw(io + REG_DATA, lo | (hi << 8));
                // ATA spec: brief delay between word writes for some drives.
                for _ in 0..4 { asm!("nop"); }
            }
            io_wait(io);
        }
        // Flush cache.
        outb(io + REG_COMMAND, CMD_CACHE_FLUSH);
        wait_ready(io)?;
    }
    Ok(())
}
