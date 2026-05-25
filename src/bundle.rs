//! Parser for the MatrixOS multi-model bundle format produced by
//! `bundle_models.py`.
//!
//! The first 64 bytes of the loaded Limine module are the bundle header.
//! If the magic doesn't match, the module is treated as a single GGUF.

use core::ptr;

pub const BUNDLE_MAGIC: &[u8; 8] = b"MTXBNDLE";
pub const MAX_MODELS:   usize    = 4;

#[derive(Copy, Clone)]
pub struct ModelEntry<'a> {
    pub name:  &'a str,        // up to 31 chars, null-terminated in the bundle
    pub data:  &'a [u8],       // slice into the bundle covering this GGUF
}

pub struct Bundle<'a> {
    pub version:   u32,
    pub count:     u32,
    pub models:    [Option<ModelEntry<'a>>; MAX_MODELS],
}

impl<'a> Bundle<'a> {
    /// Try to parse the bundle header.  Returns `None` if magic doesn't match.
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < 128 { return None; }
        if &data[0..8] != BUNDLE_MAGIC { return None; }

        let version = unsafe { ptr::read_unaligned(data.as_ptr().add(8)  as *const u32) };
        let count   = unsafe { ptr::read_unaligned(data.as_ptr().add(12) as *const u32) };
        if count == 0 || count as usize > MAX_MODELS { return None; }

        // up to 4 (offset, size) pairs starting at byte 16, 16 bytes each
        let mut models: [Option<ModelEntry<'a>>; MAX_MODELS] = [None; MAX_MODELS];

        for i in 0..count as usize {
            let off_pos  = 16 + i * 16;
            let size_pos = off_pos + 8;
            if size_pos + 8 > data.len() { return None; }
            let off  = unsafe { ptr::read_unaligned(data.as_ptr().add(off_pos)  as *const u64) } as usize;
            let size = unsafe { ptr::read_unaligned(data.as_ptr().add(size_pos) as *const u64) } as usize;
            if off + size > data.len() { return None; }

            // Name lives in a 32-byte slot starting at offset 64 + i*32
            let name_pos = 64 + i * 32;
            let name_end = (name_pos + 32).min(data.len());
            let name_slice = &data[name_pos..name_end];
            let name_len = name_slice.iter().position(|&b| b == 0).unwrap_or(name_slice.len());
            let name = core::str::from_utf8(&name_slice[..name_len]).unwrap_or("?");

            models[i] = Some(ModelEntry { name, data: &data[off..off + size] });
        }

        Some(Self { version, count, models })
    }

    pub fn get(&self, i: usize) -> Option<&ModelEntry<'a>> {
        if i >= self.count as usize { return None; }
        self.models[i].as_ref()
    }
}
