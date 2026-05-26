//! Tiny on-disk filesystem ("MFS" — MatrixOS File System).
//!
//! Layout on the first non-ATAPI IDE HDD (same disk /state writes to):
//!   LBA 100             directory sector (1 sector = 8 entries × 64 B)
//!   LBA 200..           file data, packed contiguously, sector-aligned
//!
//! Each directory entry (64 bytes):
//!   0..32     name (NUL-padded ASCII, 31 chars + terminator)
//!   32..36    u32 lba (where file starts) — 0 if free
//!   36..40    u32 size_bytes
//!   40..64    reserved
//!
//! Capacity: 8 files, up to (4096-200)*512 ≈ 1.9 MB total file data.
//! That's plenty for chat transcripts, notes, AI-generated recipes etc.
//! Lookup is linear (8 entries).

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use crate::ata;

pub const DIR_LBA:        u32 = 100;
pub const FILE_DATA_LBA:  u32 = 200;
pub const MAX_FILES:      usize = 8;
pub const ENTRY_BYTES:    usize = 64;
pub const NAME_BYTES:     usize = 32;
pub const FS_MAGIC:       &[u8; 4] = b"MFS1";

#[derive(Clone, Copy)]
pub struct DirEntry {
    pub name:       [u8; NAME_BYTES],
    pub lba:        u32,
    pub size_bytes: u32,
}

impl DirEntry {
    pub fn empty() -> Self {
        Self { name: [0; NAME_BYTES], lba: 0, size_bytes: 0 }
    }
    pub fn is_used(&self) -> bool { self.lba != 0 }
    pub fn name_str(&self) -> &str {
        let mut end = 0;
        while end < self.name.len() && self.name[end] != 0 { end += 1; }
        core::str::from_utf8(&self.name[..end]).unwrap_or("?")
    }
}

fn find_disk() -> Option<ata::DriveAddr> {
    let p = ata::probe_all();
    p.drives.iter().flatten()
        .find(|d| !d.is_atapi)
        .map(|d| d.addr)
}

/// Read the 8-entry directory.  Returns the array (always 8 entries — empty
/// ones have lba == 0).
pub fn read_dir() -> Result<[DirEntry; MAX_FILES], &'static str> {
    let addr = find_disk().ok_or("no writable disk")?;
    let mut buf = [0u8; 512];
    ata::read_sectors(addr, DIR_LBA, 1, &mut buf)?;

    let mut out = [DirEntry::empty(); MAX_FILES];
    // First 4 bytes of sector = magic, then entries starting at byte 4.
    // (Actually start at offset 64 so we have 4 entries that align nicely;
    // but for simplicity entries start at byte 0; magic check uses a special
    // "magic file" entry — see write_dir comment.  Keep it simple: just
    // pack 8 entries × 64 bytes = 512 bytes exactly.)
    // We treat name[0] == 0 && lba == 0 as "free", which is unambiguous.
    for i in 0..MAX_FILES {
        let off = i * ENTRY_BYTES;
        let mut e = DirEntry::empty();
        e.name.copy_from_slice(&buf[off..off + NAME_BYTES]);
        e.lba         = u32::from_le_bytes(buf[off + 32..off + 36].try_into().unwrap());
        e.size_bytes  = u32::from_le_bytes(buf[off + 36..off + 40].try_into().unwrap());
        out[i] = e;
    }
    Ok(out)
}

pub fn write_dir(dir: &[DirEntry; MAX_FILES]) -> Result<(), &'static str> {
    let addr = find_disk().ok_or("no writable disk")?;
    let mut buf = [0u8; 512];
    for i in 0..MAX_FILES {
        let off = i * ENTRY_BYTES;
        buf[off..off + NAME_BYTES].copy_from_slice(&dir[i].name);
        buf[off + 32..off + 36].copy_from_slice(&dir[i].lba.to_le_bytes());
        buf[off + 36..off + 40].copy_from_slice(&dir[i].size_bytes.to_le_bytes());
        // rest stays zero
    }
    ata::write_sectors(addr, DIR_LBA, 1, &buf)
}

/// Save the bytes of a file named `name`.  Up to 64 KiB per file.
pub fn save(name: &str, data: &[u8]) -> Result<(), &'static str> {
    if name.is_empty() || name.len() >= NAME_BYTES { return Err("bad name"); }
    if data.len() > 64 * 1024 { return Err("file too large (max 64 KB)"); }
    let addr = find_disk().ok_or("no writable disk")?;

    let mut dir = read_dir()?;
    // Find existing or free slot
    let mut slot: Option<usize> = None;
    for (i, e) in dir.iter().enumerate() {
        if e.is_used() && e.name_str() == name { slot = Some(i); break; }
    }
    let slot = match slot {
        Some(i) => i,
        None => dir.iter().position(|e| !e.is_used()).ok_or("directory full")?,
    };

    // Pick an LBA range.  Linear allocation: each slot owns 128 sectors
    // (64 KB) starting at FILE_DATA_LBA + slot*128.  That's simple, no
    // fragmentation, plenty for our use case.
    let lba = FILE_DATA_LBA + (slot as u32) * 128;

    // Pad to sector boundary
    let sectors_needed = ((data.len() + 511) / 512).max(1) as u8;
    if sectors_needed > 128 { return Err("file would exceed slot"); }

    let mut tmp = alloc::vec![0u8; (sectors_needed as usize) * 512];
    tmp[..data.len()].copy_from_slice(data);
    ata::write_sectors(addr, lba, sectors_needed, &tmp)?;

    // Update dir entry
    let mut e = DirEntry::empty();
    let n = name.len().min(NAME_BYTES - 1);
    e.name[..n].copy_from_slice(&name.as_bytes()[..n]);
    e.lba = lba;
    e.size_bytes = data.len() as u32;
    dir[slot] = e;
    write_dir(&dir)?;

    // Bonus: also write magic at end of dir so we know this is MFS.
    // (Optional — skip; we treat any sane-looking dir as ours.)
    let _ = FS_MAGIC;
    Ok(())
}

pub fn load(name: &str) -> Result<Vec<u8>, &'static str> {
    let dir = read_dir()?;
    let entry = dir.iter().find(|e| e.is_used() && e.name_str() == name)
        .ok_or("not found")?;
    let addr = find_disk().ok_or("no writable disk")?;
    let sectors_needed = ((entry.size_bytes as usize + 511) / 512).max(1) as u8;
    let mut buf = alloc::vec![0u8; (sectors_needed as usize) * 512];
    ata::read_sectors(addr, entry.lba, sectors_needed, &mut buf)?;
    buf.truncate(entry.size_bytes as usize);
    Ok(buf)
}

pub fn delete(name: &str) -> Result<(), &'static str> {
    let mut dir = read_dir()?;
    let idx = dir.iter().position(|e| e.is_used() && e.name_str() == name)
        .ok_or("not found")?;
    dir[idx] = DirEntry::empty();
    write_dir(&dir)?;
    Ok(())
}

/// Initialise an empty filesystem (writes a zeroed directory).
pub fn format() -> Result<(), &'static str> {
    let empty = [DirEntry::empty(); MAX_FILES];
    write_dir(&empty)?;
    Ok(())
}

/// Returns the list of (name, size) for non-empty entries.
pub fn list() -> Result<Vec<(String, u32)>, &'static str> {
    let dir = read_dir()?;
    let mut out = Vec::new();
    for e in &dir {
        if e.is_used() {
            out.push((String::from(e.name_str()), e.size_bytes));
        }
    }
    Ok(out)
}
