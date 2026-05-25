//! Persistent OS state — save/load to a fixed LBA on the first ATA HDD.
//!
//! Format (in a 4-KB region at LBA `STATE_LBA`):
//!
//!   [0..12]   magic "MATRIXOS-ST"
//!   [12..16]  u32 version (=1)
//!   [16..20]  u32 max_chat
//!   [20..24]  u32 max_code
//!   [24..28]  u32 auto_pick (0/1)
//!   [28..32]  u32 total_tokens
//!   [32..36]  u32 n_apps_saved
//!   [36..40]  u32 reserved
//!   [40..]    repeated app records:
//!                u32 name_len  (max 32)
//!                name bytes
//!                u32 author     (0/1/2/3 = builtin/chat-ai/code-ai/user)
//!                u32 text_len   (max 1024)
//!                text bytes
//!
//! Build-in apps are SKIPPED on save (they auto-install on every boot)
//! so we don't waste space.

extern crate alloc;
use alloc::string::String;
use crate::{ata, apps};

pub const STATE_LBA: u32 = 8;        // after the MBR (LBA 0) + headroom
pub const STATE_BYTES: usize = 4 * 1024;
const STATE_SECTORS: u8 = (STATE_BYTES / 512) as u8;
const MAGIC: &[u8; 12] = b"MATRIXOS-ST\0";

#[derive(Copy, Clone)]
pub struct Settings {
    pub max_chat:     u32,
    pub max_code:     u32,
    pub auto_pick:    bool,
    pub total_tokens: u32,
}

/// Pick the first non-ATAPI ATA drive.  Returns None if no HDD found.
pub fn find_target() -> Option<ata::DriveAddr> {
    let probe = ata::probe_all();
    for d in probe.drives.iter().flatten() {
        if !d.is_atapi {
            return Some(d.addr);
        }
    }
    None
}

pub fn save(settings: Settings, registry: &apps::Registry) -> Result<(), &'static str> {
    let addr = find_target().ok_or("no writable HDD found")?;
    let mut buf = [0u8; STATE_BYTES];

    buf[0..12].copy_from_slice(MAGIC);
    buf[12..16].copy_from_slice(&1u32.to_le_bytes());
    buf[16..20].copy_from_slice(&settings.max_chat.to_le_bytes());
    buf[20..24].copy_from_slice(&settings.max_code.to_le_bytes());
    buf[24..28].copy_from_slice(&(settings.auto_pick as u32).to_le_bytes());
    buf[28..32].copy_from_slice(&settings.total_tokens.to_le_bytes());

    // Skip built-ins: only save user/AI-created apps.
    let saved_apps: alloc::vec::Vec<&apps::App> = registry.list().iter()
        .filter(|a| a.author != apps::AppAuthor::Builtin)
        .collect();
    buf[32..36].copy_from_slice(&(saved_apps.len() as u32).to_le_bytes());

    let mut off = 40usize;
    for a in &saved_apps {
        let name_b = a.name.as_bytes();
        let text_b = a.text.as_bytes();
        let name_len = (name_b.len() as u32).min(32);
        let text_len = (text_b.len() as u32).min(1024);
        // Record cost: 4 + name + 4 + 4 + text
        if off + 12 + name_len as usize + text_len as usize > STATE_BYTES {
            break;   // out of room
        }
        buf[off..off+4].copy_from_slice(&name_len.to_le_bytes()); off += 4;
        buf[off..off+name_len as usize].copy_from_slice(&name_b[..name_len as usize]);
        off += name_len as usize;
        buf[off..off+4].copy_from_slice(&(a.author as u32).to_le_bytes()); off += 4;
        buf[off..off+4].copy_from_slice(&text_len.to_le_bytes()); off += 4;
        buf[off..off+text_len as usize].copy_from_slice(&text_b[..text_len as usize]);
        off += text_len as usize;
    }

    ata::write_sectors(addr, STATE_LBA, STATE_SECTORS, &buf)
}

/// Load state from disk.  Returns Some(settings) if the magic was found
/// and the apps listed inside have been registered.  Returns None on a
/// blank disk (so the kernel can fall back to defaults silently).
pub fn load(registry: &mut apps::Registry) -> Option<Settings> {
    let addr = find_target()?;
    let mut buf = [0u8; STATE_BYTES];
    ata::read_sectors(addr, STATE_LBA, STATE_SECTORS, &mut buf).ok()?;

    if &buf[0..12] != MAGIC { return None; }
    let _ver        = read_u32(&buf, 12);
    let max_chat    = read_u32(&buf, 16);
    let max_code    = read_u32(&buf, 20);
    let auto_pick   = read_u32(&buf, 24) != 0;
    let total_tokens= read_u32(&buf, 28);
    let n_apps      = read_u32(&buf, 32) as usize;

    let mut off = 40usize;
    for _ in 0..n_apps.min(apps::MAX_APPS) {
        if off + 4 > STATE_BYTES { break; }
        let name_len = read_u32(&buf, off) as usize; off += 4;
        if name_len > 32 || off + name_len > STATE_BYTES { break; }
        let name = match core::str::from_utf8(&buf[off..off + name_len]) {
            Ok(s) => String::from(s),
            Err(_) => break,
        };
        off += name_len;

        if off + 8 > STATE_BYTES { break; }
        let author_raw = read_u32(&buf, off); off += 4;
        let text_len   = read_u32(&buf, off) as usize; off += 4;
        if text_len > 1024 || off + text_len > STATE_BYTES { break; }
        let text = match core::str::from_utf8(&buf[off..off + text_len]) {
            Ok(s) => String::from(s),
            Err(_) => break,
        };
        off += text_len;

        // Re-parse and register.  Ignore failures (file might be from an
        // older DSL version).
        if let Ok(recipe) = apps::parse_recipe(&text) {
            let author = match author_raw {
                1 => apps::AppAuthor::AiChat,
                2 => apps::AppAuthor::AiCode,
                3 => apps::AppAuthor::User,
                _ => apps::AppAuthor::User,
            };
            let _ = registry.register(apps::App {
                name, recipe, author, text,
            });
        }
    }

    Some(Settings { max_chat, max_code, auto_pick, total_tokens })
}

fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]])
}
