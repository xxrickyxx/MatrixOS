// ── Framebuffer globals ───────────────────────────────────────────────────────
pub static mut FB_PTR:    *mut u8 = core::ptr::null_mut();
pub static mut FB_PITCH:  usize   = 0;
pub static mut FB_BPP:    usize   = 0;
pub static mut FB_WIDTH:  usize   = 0;
pub static mut FB_HEIGHT: usize   = 0;

// ── Terminal cursor (char units) ──────────────────────────────────────────────
pub static mut CUR_X: usize = 0;
pub static mut CUR_Y: usize = 0;

// ── Terminal bounds (pixel units) — set by init_gui() ────────────────────────
pub static mut TERM_LEFT:   usize = 0;
pub static mut TERM_TOP:    usize = 0;
pub static mut TERM_RIGHT:  usize = 1024;
pub static mut TERM_BOTTOM: usize = 768;

// ── Token counter for status bar ─────────────────────────────────────────────
pub static mut TOK_COUNT: u32 = 0;

pub const CHAR_W: usize = 9;
pub const CHAR_H: usize = 16;

// ── GUI layout constants ──────────────────────────────────────────────────────
pub const HDR_H:    usize = 36;
pub const SIDE_W:   usize = 200;
pub const STATUS_H: usize = 18;
pub const INPUT_H:  usize = 46;

// ── Palette ───────────────────────────────────────────────────────────────────
pub const C_BG:      u32 = 0x0D1117;
pub const C_SIDE:    u32 = 0x0F131A;
pub const C_HDR:     u32 = 0x161B22;
pub const C_BORDER:  u32 = 0x30363D;
pub const C_BORDER2: u32 = 0x21262D;
pub const C_GREEN:   u32 = 0x3FB950;
pub const C_CYAN:    u32 = 0x58A6FF;
pub const C_ORANGE:  u32 = 0xE3B341;
pub const C_RED:     u32 = 0xF85149;
pub const C_TEXT:    u32 = 0xC9D1D9;
pub const C_DIM:     u32 = 0x6E7681;
pub const C_USER:    u32 = 0x79C0FF;
pub const C_AI:      u32 = 0x56D364;
pub const C_BOOT:    u32 = 0xC9D1D9;

// ── 8×8 bitmap font ───────────────────────────────────────────────────────────
pub const FONT: [[u8; 8]; 128] = {
    let mut f = [[0u8; 8]; 128];
    f[b'A' as usize] = [0x18,0x24,0x42,0x42,0x7E,0x42,0x42,0x42];
    f[b'B' as usize] = [0x7C,0x42,0x42,0x7C,0x42,0x42,0x7C,0x00];
    f[b'C' as usize] = [0x3C,0x42,0x40,0x40,0x40,0x42,0x3C,0x00];
    f[b'D' as usize] = [0x78,0x44,0x42,0x42,0x42,0x44,0x78,0x00];
    f[b'E' as usize] = [0x7E,0x40,0x40,0x7C,0x40,0x40,0x7E,0x00];
    f[b'F' as usize] = [0x7E,0x40,0x40,0x7C,0x40,0x40,0x40,0x00];
    f[b'G' as usize] = [0x3C,0x42,0x40,0x4E,0x42,0x42,0x3C,0x00];
    f[b'H' as usize] = [0x42,0x42,0x42,0x7E,0x42,0x42,0x42,0x00];
    f[b'I' as usize] = [0x3C,0x18,0x18,0x18,0x18,0x18,0x3C,0x00];
    f[b'J' as usize] = [0x0E,0x04,0x04,0x04,0x04,0x44,0x38,0x00];
    f[b'K' as usize] = [0x44,0x48,0x50,0x60,0x50,0x48,0x44,0x00];
    f[b'L' as usize] = [0x40,0x40,0x40,0x40,0x40,0x40,0x7E,0x00];
    f[b'M' as usize] = [0x81,0xC3,0xA5,0x99,0x81,0x81,0x81,0x00];
    f[b'N' as usize] = [0x42,0x62,0x52,0x4A,0x46,0x42,0x42,0x00];
    f[b'O' as usize] = [0x3C,0x42,0x42,0x42,0x42,0x42,0x3C,0x00];
    f[b'P' as usize] = [0x7C,0x42,0x42,0x7C,0x40,0x40,0x40,0x00];
    f[b'Q' as usize] = [0x3C,0x42,0x42,0x42,0x4A,0x44,0x3A,0x00];
    f[b'R' as usize] = [0x7C,0x42,0x42,0x7C,0x48,0x44,0x42,0x00];
    f[b'S' as usize] = [0x3C,0x42,0x40,0x3C,0x02,0x42,0x3C,0x00];
    f[b'T' as usize] = [0x7E,0x18,0x18,0x18,0x18,0x18,0x18,0x00];
    f[b'U' as usize] = [0x42,0x42,0x42,0x42,0x42,0x42,0x3C,0x00];
    f[b'V' as usize] = [0x42,0x42,0x42,0x42,0x24,0x24,0x18,0x00];
    f[b'W' as usize] = [0x81,0x81,0x81,0x99,0xA5,0xC3,0x81,0x00];
    f[b'X' as usize] = [0x42,0x42,0x24,0x18,0x24,0x42,0x42,0x00];
    f[b'Y' as usize] = [0x42,0x42,0x24,0x18,0x18,0x18,0x18,0x00];
    f[b'Z' as usize] = [0x7E,0x02,0x04,0x08,0x10,0x20,0x7E,0x00];
    f[b'a' as usize] = [0x00,0x00,0x3C,0x02,0x3E,0x42,0x3F,0x00];
    f[b'b' as usize] = [0x40,0x40,0x5C,0x62,0x42,0x62,0x5C,0x00];
    f[b'c' as usize] = [0x00,0x00,0x3C,0x42,0x40,0x42,0x3C,0x00];
    f[b'd' as usize] = [0x02,0x02,0x3A,0x46,0x42,0x46,0x3B,0x00];
    f[b'e' as usize] = [0x00,0x00,0x3C,0x42,0x7E,0x40,0x3C,0x00];
    f[b'f' as usize] = [0x0C,0x12,0x10,0x38,0x10,0x10,0x10,0x00];
    f[b'g' as usize] = [0x00,0x00,0x3E,0x42,0x42,0x3E,0x02,0x3C];
    f[b'h' as usize] = [0x40,0x40,0x5C,0x62,0x42,0x42,0x42,0x00];
    f[b'i' as usize] = [0x18,0x00,0x18,0x18,0x18,0x18,0x18,0x00];
    f[b'j' as usize] = [0x06,0x00,0x06,0x06,0x06,0x46,0x3C,0x00];
    f[b'k' as usize] = [0x40,0x40,0x44,0x48,0x70,0x48,0x44,0x00];
    f[b'l' as usize] = [0x18,0x18,0x18,0x18,0x18,0x18,0x0C,0x00];
    f[b'm' as usize] = [0x00,0x00,0x76,0x49,0x49,0x49,0x49,0x00];
    f[b'n' as usize] = [0x00,0x00,0x5C,0x62,0x42,0x42,0x42,0x00];
    f[b'o' as usize] = [0x00,0x00,0x3C,0x42,0x42,0x42,0x3C,0x00];
    f[b'p' as usize] = [0x00,0x00,0x5C,0x62,0x62,0x5C,0x40,0x40];
    f[b'q' as usize] = [0x00,0x00,0x3A,0x46,0x46,0x3A,0x02,0x02];
    f[b'r' as usize] = [0x00,0x00,0x5C,0x62,0x40,0x40,0x40,0x00];
    f[b's' as usize] = [0x00,0x00,0x3C,0x40,0x3C,0x02,0x3C,0x00];
    f[b't' as usize] = [0x10,0x10,0x7C,0x10,0x10,0x12,0x0C,0x00];
    f[b'u' as usize] = [0x00,0x00,0x42,0x42,0x42,0x46,0x3A,0x00];
    f[b'v' as usize] = [0x00,0x00,0x42,0x42,0x42,0x24,0x18,0x00];
    f[b'w' as usize] = [0x00,0x00,0x49,0x49,0x49,0x49,0x36,0x00];
    f[b'x' as usize] = [0x00,0x00,0x42,0x24,0x18,0x24,0x42,0x00];
    f[b'y' as usize] = [0x00,0x00,0x42,0x42,0x3E,0x02,0x3C,0x00];
    f[b'z' as usize] = [0x00,0x00,0x7E,0x04,0x18,0x20,0x7E,0x00];
    f[b'0' as usize] = [0x3C,0x42,0x46,0x4A,0x52,0x62,0x3C,0x00];
    f[b'1' as usize] = [0x18,0x38,0x18,0x18,0x18,0x18,0x3C,0x00];
    f[b'2' as usize] = [0x3C,0x42,0x02,0x0C,0x30,0x40,0x7E,0x00];
    f[b'3' as usize] = [0x3C,0x42,0x02,0x1C,0x02,0x42,0x3C,0x00];
    f[b'4' as usize] = [0x08,0x18,0x28,0x48,0x7E,0x08,0x08,0x00];
    f[b'5' as usize] = [0x7E,0x40,0x7C,0x02,0x02,0x42,0x3C,0x00];
    f[b'6' as usize] = [0x3C,0x40,0x7C,0x42,0x42,0x42,0x3C,0x00];
    f[b'7' as usize] = [0x7E,0x02,0x04,0x08,0x10,0x10,0x10,0x00];
    f[b'8' as usize] = [0x3C,0x42,0x42,0x3C,0x42,0x42,0x3C,0x00];
    f[b'9' as usize] = [0x3C,0x42,0x42,0x3E,0x02,0x42,0x3C,0x00];
    f[b' '  as usize] = [0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00];
    f[b'.'  as usize] = [0x00,0x00,0x00,0x00,0x00,0x18,0x18,0x00];
    f[b','  as usize] = [0x00,0x00,0x00,0x00,0x00,0x18,0x18,0x08];
    f[b'!'  as usize] = [0x18,0x18,0x18,0x18,0x18,0x00,0x18,0x00];
    f[b'?'  as usize] = [0x3C,0x42,0x02,0x0C,0x18,0x00,0x18,0x00];
    f[b':'  as usize] = [0x00,0x18,0x18,0x00,0x00,0x18,0x18,0x00];
    f[b';'  as usize] = [0x00,0x18,0x18,0x00,0x00,0x18,0x18,0x08];
    f[b'-'  as usize] = [0x00,0x00,0x00,0x3C,0x00,0x00,0x00,0x00];
    f[b'_'  as usize] = [0x00,0x00,0x00,0x00,0x00,0x00,0x7E,0x00];
    f[b'+'  as usize] = [0x00,0x08,0x08,0x3E,0x08,0x08,0x00,0x00];
    f[b'='  as usize] = [0x00,0x00,0x7E,0x00,0x7E,0x00,0x00,0x00];
    f[b'/'  as usize] = [0x02,0x04,0x08,0x10,0x20,0x40,0x00,0x00];
    f[b'\\'  as usize] = [0x40,0x20,0x10,0x08,0x04,0x02,0x00,0x00];
    f[b'('  as usize] = [0x0C,0x18,0x30,0x30,0x30,0x18,0x0C,0x00];
    f[b')'  as usize] = [0x30,0x18,0x0C,0x0C,0x0C,0x18,0x30,0x00];
    f[b'['  as usize] = [0x3C,0x20,0x20,0x20,0x20,0x20,0x3C,0x00];
    f[b']'  as usize] = [0x3C,0x04,0x04,0x04,0x04,0x04,0x3C,0x00];
    f[b'<'  as usize] = [0x04,0x08,0x10,0x20,0x10,0x08,0x04,0x00];
    f[b'>'  as usize] = [0x20,0x10,0x08,0x04,0x08,0x10,0x20,0x00];
    f[b'\'' as usize] = [0x18,0x18,0x08,0x00,0x00,0x00,0x00,0x00];
    f[b'"'  as usize] = [0x24,0x24,0x12,0x00,0x00,0x00,0x00,0x00];
    f[b'#'  as usize] = [0x24,0x24,0x7E,0x24,0x7E,0x24,0x24,0x00];
    f[b'@'  as usize] = [0x3C,0x42,0x5E,0x52,0x5E,0x40,0x3C,0x00];
    f[b'*'  as usize] = [0x00,0x24,0x18,0x7E,0x18,0x24,0x00,0x00];
    f[b'%'  as usize] = [0x62,0x64,0x08,0x10,0x26,0x46,0x00,0x00];
    f[b'&'  as usize] = [0x38,0x44,0x44,0x38,0x4A,0x44,0x3A,0x00];
    f[b'^'  as usize] = [0x10,0x28,0x44,0x00,0x00,0x00,0x00,0x00];
    f[b'~'  as usize] = [0x00,0x00,0x4C,0x32,0x00,0x00,0x00,0x00];
    f[b'|'  as usize] = [0x18,0x18,0x18,0x18,0x18,0x18,0x18,0x00];
    f[b'`'  as usize] = [0x18,0x18,0x10,0x00,0x00,0x00,0x00,0x00];
    f
};

// ── Low-level pixel primitives ────────────────────────────────────────────────

#[inline(always)]
fn put_pixel(x: usize, y: usize, r: u8, g: u8, b: u8) {
    unsafe {
        if FB_PTR.is_null() || x >= FB_WIDTH || y >= FB_HEIGHT { return; }
        let off = y * FB_PITCH + x * FB_BPP;
        *FB_PTR.add(off)     = b;
        *FB_PTR.add(off + 1) = g;
        *FB_PTR.add(off + 2) = r;
    }
}

pub fn fill_rect(x: usize, y: usize, w: usize, h: usize, color: u32) {
    let r = ((color >> 16) & 0xFF) as u8;
    let g = ((color >>  8) & 0xFF) as u8;
    let b = (color & 0xFF) as u8;
    unsafe {
        if FB_PTR.is_null() { return; }
        let x1 = (x + w).min(FB_WIDTH);
        let y1 = (y + h).min(FB_HEIGHT);
        for py in y..y1 {
            let row_off = py * FB_PITCH;
            for px in x..x1 {
                let off = row_off + px * FB_BPP;
                *FB_PTR.add(off)     = b;
                *FB_PTR.add(off + 1) = g;
                *FB_PTR.add(off + 2) = r;
            }
        }
    }
}

pub fn draw_hline(x: usize, y: usize, w: usize, color: u32) {
    fill_rect(x, y, w, 1, color);
}

pub fn draw_vline(x: usize, y: usize, h: usize, color: u32) {
    fill_rect(x, y, 1, h, color);
}

/// Draw a rectangle border (1px thick).
pub fn draw_rect_border(x: usize, y: usize, w: usize, h: usize, color: u32) {
    draw_hline(x,         y,         w, color);
    draw_hline(x,         y + h - 1, w, color);
    draw_vline(x,         y,         h, color);
    draw_vline(x + w - 1, y,         h, color);
}

/// Draw a character on the raw framebuffer.
pub fn draw_char(fb: *mut u8, pitch: usize, bpp: usize, x: usize, y: usize, c: u8, color: u32) {
    if c >= 128 { return; }
    let glyph = FONT[c as usize];
    let r = ((color >> 16) & 0xFF) as u8;
    let g = ((color >>  8) & 0xFF) as u8;
    let b = (color & 0xFF) as u8;
    for row in 0..8 {
        for col in 0..8 {
            if (glyph[row] >> (7 - col)) & 1 == 1 {
                let offset = (y + row) * pitch + (x + col) * bpp;
                unsafe {
                    *fb.add(offset)     = b;
                    *fb.add(offset + 1) = g;
                    *fb.add(offset + 2) = r;
                }
            }
        }
    }
}

/// Draw a string to an arbitrary framebuffer position.
pub fn draw_string(fb: *mut u8, pitch: usize, bpp: usize, x: usize, y: usize, s: &str, color: u32) {
    let mut cx = x;
    for &byte in s.as_bytes() {
        if byte < 128 {
            draw_char(fb, pitch, bpp, cx, y, byte, color);
        }
        cx += CHAR_W;
    }
}

pub fn draw_hex(fb: *mut u8, pitch: usize, bpp: usize, x: usize, y: usize, val: u64, color: u32) {
    let mut buf = [b'0'; 18];
    buf[0] = b'0'; buf[1] = b'x';
    let mut tmp = val;
    for i in (0..16).rev() {
        let d = (tmp & 0xF) as u8;
        buf[i + 2] = if d < 10 { b'0' + d } else { b'A' + (d - 10) };
        tmp >>= 4;
    }
    if let Ok(s) = core::str::from_utf8(&buf) {
        draw_string(fb, pitch, bpp, x, y, s, color);
    }
}

/// Draw a string to the screen using the global FB.
fn draw_str_at(x: usize, y: usize, s: &str, color: u32) {
    unsafe { draw_string(FB_PTR, FB_PITCH, FB_BPP, x, y, s, color); }
}

// ── GUI initialisation ────────────────────────────────────────────────────────

pub struct SidebarInfo<'a> {
    pub model_name:    &'a str,
    pub quant:         &'a str,
    pub n_tensors:     u32,
    pub d_model:       u32,
    pub n_layers:      u32,
    pub n_heads:       u32,
    pub n_kv_heads:    u32,
    pub head_dim:      u32,
    pub ffn_dim:       u32,
    pub vocab:         u32,
    // Multi-model fields. If only one is loaded, chat_name == code_name == model_name.
    pub chat_name:     &'a str,
    pub code_name:     &'a str,
    pub active_slot:   u8,        // 0 = chat, 1 = code
    pub nic_info:      &'a str,
    pub usb_info:      &'a str,
    pub gpu_info:      &'a str,
    pub storage_info:  &'a str,
    pub isa:           &'a str,
    pub heap_mb:       u32,
}

// Remember screen positions of the model labels so we can repaint just
// the active highlight when the user hits F1/F2.
pub static mut MODE_CHAT_X: usize = 0;
pub static mut MODE_CHAT_Y: usize = 0;
pub static mut MODE_CODE_X: usize = 0;
pub static mut MODE_CODE_Y: usize = 0;
pub static mut MODE_CHAT_LEN: usize = 0;
pub static mut MODE_CODE_LEN: usize = 0;

pub fn init_terminal(fb: *mut u8, pitch: usize, bpp: usize, width: usize, height: usize) {
    unsafe {
        FB_PTR    = fb;
        FB_PITCH  = pitch;
        FB_BPP    = bpp;
        FB_WIDTH  = width;
        FB_HEIGHT = height;
        // Default: full screen terminal (pre-GUI boot)
        TERM_LEFT   = 4;
        TERM_TOP    = 4;
        TERM_RIGHT  = width;
        TERM_BOTTOM = height;
        CUR_X = 0;
        CUR_Y = 0;
    }
}

/// Draw the full GUI chrome and set terminal bounds to the chat area.
pub fn init_gui(info: &SidebarInfo) {
    unsafe {
        if FB_PTR.is_null() { return; }
        let w = FB_WIDTH;
        let h = FB_HEIGHT;

        // ── Background ──────────────────────────────────────────────────────
        fill_rect(0, 0, w, h, C_BG);

        // ── Header bar ──────────────────────────────────────────────────────
        fill_rect(0, 0, w, HDR_H, C_HDR);
        draw_hline(0, HDR_H - 1, w, C_BORDER);
        // Logo left
        draw_str_at(12, 10, "MATRIX", C_GREEN);
        draw_str_at(12 + 6*CHAR_W, 10, "OS", C_CYAN);
        draw_str_at(12 + 8*CHAR_W + 6, 10, "v3.0", C_DIM);
        // Title centre
        let title = "BARE-METAL LLM OPERATING SYSTEM";
        let tx = w / 2 - title.len() * CHAR_W / 2;
        draw_str_at(tx, 10, title, C_DIM);
        // Live indicator right
        let live_x = w.saturating_sub(80);
        fill_rect(live_x, 12, 8, 8, C_GREEN);
        draw_str_at(live_x + 12, 10, "LIVE", C_GREEN);

        // ── Sidebar ─────────────────────────────────────────────────────────
        let sidebar_h = h.saturating_sub(HDR_H + STATUS_H);
        fill_rect(0, HDR_H, SIDE_W, sidebar_h, C_SIDE);
        draw_vline(SIDE_W, HDR_H, sidebar_h, C_BORDER);

        // Sidebar sections
        let sx = 10usize;
        let mut sy = HDR_H + 12;

        // ─ MODELS ─ (two slots, current one highlighted)
        draw_hline(sx, sy + 10, SIDE_W - sx - 6, C_BORDER2);
        draw_str_at(sx, sy, "MODELS", C_CYAN);
        sy += 20;
        // Chat slot
        let chat_color = if info.active_slot == 0 { C_GREEN } else { C_DIM };
        let chat_mark  = if info.active_slot == 0 { ">" } else { " " };
        draw_str_at(sx, sy, chat_mark, chat_color);
        draw_str_at(sx + 2*CHAR_W, sy, "CHAT", C_CYAN);
        sy += CHAR_H;
        MODE_CHAT_X   = sx + 2*CHAR_W;
        MODE_CHAT_Y   = sy;
        MODE_CHAT_LEN = info.chat_name.len();
        draw_str_at(sx + 2*CHAR_W, sy, info.chat_name, chat_color); sy += CHAR_H + 4;
        // Code slot
        let code_color = if info.active_slot == 1 { C_GREEN } else { C_DIM };
        let code_mark  = if info.active_slot == 1 { ">" } else { " " };
        draw_str_at(sx, sy, code_mark, code_color);
        draw_str_at(sx + 2*CHAR_W, sy, "CODE", C_ORANGE);
        sy += CHAR_H;
        MODE_CODE_X   = sx + 2*CHAR_W;
        MODE_CODE_Y   = sy;
        MODE_CODE_LEN = info.code_name.len();
        draw_str_at(sx + 2*CHAR_W, sy, info.code_name, code_color); sy += CHAR_H + 6;
        // Active model details follow
        draw_str_at(sx, sy, "ACTIVE", C_CYAN); sy += CHAR_H + 2;
        draw_str_at(sx, sy, info.quant,      C_ORANGE); sy += CHAR_H + 2;

        // tensors label
        let mut nbuf = [0u8; 6];
        let ns = u32_to_str(info.n_tensors, &mut nbuf);
        draw_str_at(sx, sy, "Tensors: ", C_DIM);
        draw_str_at(sx + 9*CHAR_W, sy, ns, C_TEXT); sy += CHAR_H + 2;

        // d_model
        let mut db = [0u8; 6];
        let ds = u32_to_str(info.d_model, &mut db);
        draw_str_at(sx, sy, "d_model: ", C_DIM);
        draw_str_at(sx + 9*CHAR_W, sy, ds, C_TEXT); sy += CHAR_H + 2;

        // layers
        let mut lb = [0u8; 4];
        let ls = u32_to_str(info.n_layers, &mut lb);
        draw_str_at(sx, sy, "Layers:  ", C_DIM);
        draw_str_at(sx + 9*CHAR_W, sy, ls, C_TEXT); sy += CHAR_H + 2;

        // heads (Q / KV)
        let mut hb1 = [0u8; 4]; let mut hb2 = [0u8; 4];
        let hs1 = u32_to_str(info.n_heads,    &mut hb1);
        let hs2 = u32_to_str(info.n_kv_heads, &mut hb2);
        draw_str_at(sx, sy, "Heads:   ", C_DIM);
        draw_str_at(sx + 9*CHAR_W, sy, hs1, C_TEXT);
        draw_str_at(sx + 9*CHAR_W + (hs1.len() as usize) * CHAR_W, sy, "/", C_DIM);
        draw_str_at(sx + 9*CHAR_W + ((hs1.len() as usize)+1) * CHAR_W, sy, hs2, C_TEXT);
        sy += CHAR_H + 2;

        // head_dim
        let mut hdb = [0u8; 4];
        let hds = u32_to_str(info.head_dim, &mut hdb);
        draw_str_at(sx, sy, "HeadDim: ", C_DIM);
        draw_str_at(sx + 9*CHAR_W, sy, hds, C_TEXT); sy += CHAR_H + 2;

        // ffn_dim
        let mut fb = [0u8; 8];
        let fs = u32_to_str(info.ffn_dim, &mut fb);
        draw_str_at(sx, sy, "FFN:     ", C_DIM);
        draw_str_at(sx + 9*CHAR_W, sy, fs, C_TEXT); sy += CHAR_H + 2;

        // vocab
        let mut vb = [0u8; 8];
        let vs = u32_to_str(info.vocab, &mut vb);
        draw_str_at(sx, sy, "Vocab:   ", C_DIM);
        draw_str_at(sx + 9*CHAR_W, sy, vs, C_TEXT); sy += CHAR_H + 8;

        // ─ SYSTEM ─
        draw_hline(sx, sy + 10, SIDE_W - sx - 6, C_BORDER2);
        draw_str_at(sx, sy, "SYSTEM", C_CYAN); sy += 20;
        draw_str_at(sx, sy, "Heap:",         C_DIM);
        let mut hb = [0u8; 6];
        let hs = u32_to_str(info.heap_mb, &mut hb);
        draw_str_at(sx + 5*CHAR_W + 3, sy, hs, C_TEXT);
        draw_str_at(sx + 5*CHAR_W + 3 + hs.len()*CHAR_W, sy, " MB", C_DIM);
        sy += CHAR_H + 2;
        draw_str_at(sx, sy, "ISA: ",         C_DIM);
        draw_str_at(sx + 5*CHAR_W, sy, info.isa, C_TEXT); sy += CHAR_H + 2;
        draw_str_at(sx, sy, "NIC:", C_DIM);
        draw_str_at(sx + 5*CHAR_W, sy, info.nic_info, C_TEXT); sy += CHAR_H + 2;
        draw_str_at(sx, sy, "USB:", C_DIM);
        draw_str_at(sx + 5*CHAR_W, sy, info.usb_info, C_TEXT); sy += CHAR_H + 2;
        draw_str_at(sx, sy, "GPU:", C_DIM);
        draw_str_at(sx + 5*CHAR_W, sy, info.gpu_info, C_TEXT); sy += CHAR_H + 2;
        draw_str_at(sx, sy, "Disk:", C_DIM);
        draw_str_at(sx + 6*CHAR_W, sy, info.storage_info, C_TEXT); sy += CHAR_H + 8;

        // ─ CONTROLS ─
        draw_hline(sx, sy + 10, SIDE_W - sx - 6, C_BORDER2);
        draw_str_at(sx, sy, "CONTROLS", C_CYAN); sy += 20;
        draw_str_at(sx, sy, "ENTER  send",    C_DIM); sy += CHAR_H + 2;
        draw_str_at(sx, sy, "F1 chat mode",   C_DIM); sy += CHAR_H + 2;
        draw_str_at(sx, sy, "F2 code mode",   C_DIM); sy += CHAR_H + 2;
        draw_str_at(sx, sy, "F3 swap model",  C_DIM); sy += CHAR_H + 2;
        draw_str_at(sx, sy, "F4 reset other", C_DIM); sy += CHAR_H + 2;
        draw_str_at(sx, sy, "TAB auto-pick",  C_DIM); sy += CHAR_H + 2;
        draw_str_at(sx, sy, "F5 clear chat",  C_DIM); sy += CHAR_H + 2;
        draw_str_at(sx, sy, "BKSP  delete",   C_DIM);

        // ── Status bar ───────────────────────────────────────────────────────
        let sbar_y = h.saturating_sub(STATUS_H);
        fill_rect(0, sbar_y, w, STATUS_H, C_HDR);
        draw_hline(0, sbar_y, w, C_BORDER);
        draw_str_at(8, sbar_y + 4, "MatrixOS v3.0", C_DIM);
        draw_vline(SIDE_W, sbar_y, STATUS_H, C_BORDER);
        draw_str_at(SIDE_W + 8, sbar_y + 4, info.model_name, C_DIM);
        let name_w = info.model_name.len() * CHAR_W;
        draw_str_at(SIDE_W + 8 + name_w + CHAR_W, sbar_y + 4, info.quant, C_DIM);

        // ── Input area ───────────────────────────────────────────────────────
        let input_y = h.saturating_sub(STATUS_H + INPUT_H);
        fill_rect(SIDE_W + 1, input_y, w - SIDE_W - 1, INPUT_H, C_SIDE);
        draw_hline(SIDE_W + 1, input_y, w - SIDE_W - 1, C_BORDER);
        // Prompt prefix
        draw_str_at(SIDE_W + 10, input_y + 14, "USER", C_USER);
        draw_str_at(SIDE_W + 10 + 4*CHAR_W, input_y + 14, ">", C_BORDER);

        // ── Set terminal bounds to chat area ─────────────────────────────────
        let chat_top    = HDR_H + 4;
        let chat_bottom = input_y.saturating_sub(2);
        let chat_left   = SIDE_W + 6;
        let chat_right  = w.saturating_sub(4);
        TERM_LEFT   = chat_left;
        TERM_TOP    = chat_top;
        TERM_RIGHT  = chat_right;
        TERM_BOTTOM = chat_bottom;
        CUR_X = 0;
        CUR_Y = 0;
    }
}

/// Re-paint the sidebar's CHAT/CODE highlights when the active model
/// changes at runtime (F1/F2 hotkeys).
pub fn set_active_mode(slot: u8, chat_name: &str, code_name: &str) {
    unsafe {
        if FB_PTR.is_null() { return; }
        let sx = 10usize;
        // Repaint the chat row
        let chat_color = if slot == 0 { C_GREEN } else { C_DIM };
        let chat_mark  = if slot == 0 { ">" } else { " " };
        let chat_y     = MODE_CHAT_Y - CHAR_H;
        // Clear marker column
        fill_rect(sx, chat_y, CHAR_W, CHAR_H, C_SIDE);
        draw_str_at(sx, chat_y, chat_mark, chat_color);
        // Repaint chat name row
        let row_w = SIDE_W.saturating_sub(MODE_CHAT_X + 4);
        fill_rect(MODE_CHAT_X, MODE_CHAT_Y, row_w, CHAR_H, C_SIDE);
        draw_str_at(MODE_CHAT_X, MODE_CHAT_Y, chat_name, chat_color);

        // Repaint the code row
        let code_color = if slot == 1 { C_GREEN } else { C_DIM };
        let code_mark  = if slot == 1 { ">" } else { " " };
        let code_y     = MODE_CODE_Y - CHAR_H;
        fill_rect(sx, code_y, CHAR_W, CHAR_H, C_SIDE);
        draw_str_at(sx, code_y, code_mark, code_color);
        let row_w2 = SIDE_W.saturating_sub(MODE_CODE_X + 4);
        fill_rect(MODE_CODE_X, MODE_CODE_Y, row_w2, CHAR_H, C_SIDE);
        draw_str_at(MODE_CODE_X, MODE_CODE_Y, code_name, code_color);
    }
}

/// Wipe the chat-area scrollback to a blank screen, repaint the input row.
pub fn clear_chat() {
    unsafe {
        if FB_PTR.is_null() { return; }
        let left   = TERM_LEFT;
        let top    = TERM_TOP;
        let right  = TERM_RIGHT.min(FB_WIDTH);
        let bottom = TERM_BOTTOM.min(FB_HEIGHT);
        if right > left && bottom > top {
            fill_rect(left, top, right - left, bottom - top, C_BG);
        }
        CUR_X = 0;
        CUR_Y = 0;
    }
}

/// Paint a live "Up Xs" clock on the LEFT half of the status bar.
/// Cheap: only updates when the second-integer changed.
pub fn update_status_clock(secs: u32) {
    static mut LAST_SECS: u32 = u32::MAX;
    unsafe {
        if LAST_SECS == secs { return; }
        LAST_SECS = secs;
        if FB_PTR.is_null() { return; }
        let h = FB_HEIGHT;
        let w = FB_WIDTH;
        let sbar_y = h.saturating_sub(STATUS_H);
        let _ = w;
        // Clear the clock area (left ~140 px starting at sidebar edge)
        let cx = SIDE_W + 8;
        fill_rect(cx, sbar_y + 1, 140, STATUS_H - 2, C_HDR);
        draw_str_at(cx, sbar_y + 4, "Up", C_DIM);
        let mut bs = [0u8; 12];
        let ss = u32_to_str(secs, &mut bs);
        draw_str_at(cx + 3*CHAR_W, sbar_y + 4, ss, C_GREEN);
        draw_str_at(cx + (3 + ss.len()) * CHAR_W, sbar_y + 4, "s", C_DIM);
    }
}

/// Update the status bar token count + optional tok/s rate.
/// `toks_per_sec == 0` means "not yet measured, keep previous rate".
pub fn update_status_tokens(n: u32, toks_per_sec: u32) {
    unsafe {
        TOK_COUNT = n;
        if FB_PTR.is_null() { return; }
        let h      = FB_HEIGHT;
        let w      = FB_WIDTH;
        let sbar_y = h.saturating_sub(STATUS_H);
        // Clear the token+rate area (right 220px)
        let tok_x = w.saturating_sub(220);
        fill_rect(tok_x, sbar_y + 1, 218, STATUS_H - 2, C_HDR);

        // "Tok: NNNN  ~XX t/s"
        draw_str_at(tok_x, sbar_y + 4, "Tok:", C_DIM);
        let mut b = [0u8; 8];
        let s = u32_to_str(n, &mut b);
        draw_str_at(tok_x + 5*CHAR_W, sbar_y + 4, s, C_GREEN);

        if toks_per_sec > 0 {
            let rate_x = tok_x + 11*CHAR_W;
            draw_str_at(rate_x, sbar_y + 4, "~", C_DIM);
            let mut rb = [0u8; 8];
            let rs = u32_to_str(toks_per_sec, &mut rb);
            draw_str_at(rate_x + CHAR_W, sbar_y + 4, rs, C_CYAN);
            draw_str_at(rate_x + CHAR_W + rs.len() * CHAR_W, sbar_y + 4, "t/s", C_DIM);
        }
    }
}

/// Draw (or refresh) the input line. `buf` is the current text, `len` the length.
pub fn draw_input_line(buf: &[u8], len: usize) {
    unsafe {
        if FB_PTR.is_null() { return; }
        let h       = FB_HEIGHT;
        let w       = FB_WIDTH;
        let input_y = h.saturating_sub(STATUS_H + INPUT_H);
        let text_x  = SIDE_W + 10 + 6 * CHAR_W; // after "USER> "
        let text_w  = w.saturating_sub(text_x + 20);

        // Clear text area
        fill_rect(text_x, input_y + 6, text_w, CHAR_H + 4, C_SIDE);

        // Draw typed text
        for i in 0..len.min(text_w / CHAR_W) {
            let c = if buf[i] >= 32 && buf[i] < 128 { buf[i] } else { b'?' };
            let cx = text_x + i * CHAR_W;
            draw_char(FB_PTR, FB_PITCH, FB_BPP, cx, input_y + 8, c, C_TEXT);
        }
        // Cursor
        let cur_x = text_x + len.min(text_w / CHAR_W) * CHAR_W;
        fill_rect(cur_x, input_y + 8, 6, CHAR_H - 2, C_GREEN);
    }
}

// ── Terminal functions ────────────────────────────────────────────────────────

pub fn scroll_up() {
    unsafe {
        if FB_PTR.is_null() { return; }
        let left   = TERM_LEFT;
        let top    = TERM_TOP;
        let bottom = TERM_BOTTOM;
        let right  = TERM_RIGHT.min(FB_WIDTH);
        if bottom <= top + CHAR_H { return; }

        // Scroll rows up, only within the chat column range.
        let h = bottom.saturating_sub(top);
        for row_off in 0..h.saturating_sub(CHAR_H) {
            let sy = top + row_off + CHAR_H;
            let dy = top + row_off;
            if sy >= FB_HEIGHT { break; }
            for px in left..right {
                let src = sy * FB_PITCH + px * FB_BPP;
                let dst = dy * FB_PITCH + px * FB_BPP;
                for b in 0..FB_BPP {
                    *FB_PTR.add(dst + b) = *FB_PTR.add(src + b);
                }
            }
        }
        // Clear the last row in the chat column range
        let clear_y = bottom.saturating_sub(CHAR_H);
        fill_rect(left, clear_y, right.saturating_sub(left), CHAR_H, C_BG);
    }
}

/// Print a single char to the terminal (bounded by TERM_LEFT/RIGHT/TOP/BOTTOM).
pub fn term_putc(c: u8, color: u32) {
    unsafe {
        if FB_PTR.is_null() { return; }
        let max_cols = (TERM_RIGHT.saturating_sub(TERM_LEFT)) / CHAR_W;
        let max_rows = (TERM_BOTTOM.saturating_sub(TERM_TOP))  / CHAR_H;
        if max_cols == 0 || max_rows == 0 { return; }

        match c {
            b'\n' | b'\r' => {
                CUR_X = 0;
                CUR_Y += 1;
            }
            0x08 => {
                // backspace
                if CUR_X > 0 {
                    CUR_X -= 1;
                    let px = TERM_LEFT + CUR_X * CHAR_W;
                    let py = TERM_TOP  + CUR_Y * CHAR_H;
                    fill_rect(px, py, CHAR_W, CHAR_H, C_BG);
                }
            }
            _ if c >= 32 => {
                let px = TERM_LEFT + CUR_X * CHAR_W;
                let py = TERM_TOP  + CUR_Y * CHAR_H;
                draw_char(FB_PTR, FB_PITCH, FB_BPP, px, py, c, color);
                CUR_X += 1;
                if CUR_X >= max_cols {
                    CUR_X = 0;
                    CUR_Y += 1;
                }
            }
            _ => {}
        }

        if CUR_Y >= max_rows {
            scroll_up();
            CUR_Y = max_rows - 1;
        }
    }
}

pub fn term_puts(s: &str, color: u32) {
    for &b in s.as_bytes() { term_putc(b, color); }
}

pub fn term_putu32(n: u32, color: u32) {
    let mut buf = [0u8; 10];
    let s = u32_to_str(n, &mut buf);
    term_puts(s, color);
}

/// Print a single byte as two hex digits (e.g. 0xAB → "AB").
pub fn term_puthex8(b: u8, color: u32) {
    const HEX: &[u8] = b"0123456789ABCDEF";
    term_putc(HEX[(b >> 4) as usize], color);
    term_putc(HEX[(b & 0xF) as usize], color);
}

pub fn term_newline() { term_putc(b'\n', 0); }

// ── Panic helper ─────────────────────────────────────────────────────────────

pub fn emergency_fill(color: u32) {
    unsafe {
        if FB_PTR.is_null() { return; }
        let r = ((color >> 16) & 0xFF) as u8;
        let g = ((color >>  8) & 0xFF) as u8;
        let b = (color & 0xFF) as u8;
        let total = FB_HEIGHT * FB_PITCH;
        let mut i = 0usize;
        while i + 2 < total {
            *FB_PTR.add(i)     = b;
            *FB_PTR.add(i + 1) = g;
            *FB_PTR.add(i + 2) = r;
            i += FB_BPP.max(3);
        }
    }
}

// ── Utilities ─────────────────────────────────────────────────────────────────

/// Write decimal representation of n into buf; returns the slice of digits.
pub fn u32_to_str<'a>(mut n: u32, buf: &'a mut [u8]) -> &'a str {
    if n == 0 {
        if !buf.is_empty() { buf[0] = b'0'; }
        return core::str::from_utf8(&buf[..1]).unwrap_or("0");
    }
    let len = buf.len();
    let mut i = len;
    while n > 0 && i > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    core::str::from_utf8(&buf[i..len]).unwrap_or("?")
}
