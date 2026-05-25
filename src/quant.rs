/// F16 (IEEE 754 half-precision) to F32 conversion (software, no hardware needed).
#[inline(always)]
pub fn f16_to_f32(bits: u16) -> f32 {
    let exp  = ((bits >> 10) & 0x1F) as i32;
    let mant = (bits & 0x3FF) as u32;
    let sign = ((bits >> 15) as u32) << 31;
    let val: u32 = if exp == 0 {
        if mant == 0 { sign } else {
            // Denormalized
            let mut m = mant;
            let mut e = 0u32;
            while m & 0x400 == 0 { m <<= 1; e += 1; }
            m &= 0x3FF;
            sign | ((127 - 15 - e + 1) << 23) | (m << 13)
        }
    } else if exp == 31 {
        if mant == 0 { sign | 0x7F800000 } else { sign | 0x7FC00000 } // inf / nan
    } else {
        sign | (((exp + 127 - 15) as u32) << 23) | (mant << 13)
    };
    f32::from_bits(val)
}

// ── Q4_K (type 16 / 17)  block = 256 weights, 144 bytes ──────────────────────
//   [0..2]  d    (f16 super-block scale for scales)
//   [2..4]  dmin (f16 super-block scale for mins)
//   [4..16] scales[12]
//   [16..144] qs[128]  4-bit packed values

#[inline(always)]
fn get_scale_min_k4(j: usize, scales: &[u8]) -> (f32, f32) {
    let (sc, m) = if j < 4 {
        (scales[j] & 63, scales[j + 4] & 63)
    } else {
        let sc = (scales[j + 4] & 0x0F) | ((scales[j - 4] >> 6) << 4);
        let m  = (scales[j + 4] >>   4) | ((scales[j    ] >> 6) << 4);
        (sc, m)
    };
    (sc as f32, m as f32)
}

/// Dequantize one Q4_K block (144 bytes) into `out` (256 f32 values).
pub fn dequant_q4k_block(block: &[u8], out: &mut [f32]) {
    debug_assert!(block.len() >= 144);
    let d    = f16_to_f32(u16::from_le_bytes([block[0], block[1]]));
    let dmin = f16_to_f32(u16::from_le_bytes([block[2], block[3]]));
    let scales = &block[4..16];
    let qs     = &block[16..144];

    let mut out_i = 0usize;
    let mut q_i   = 0usize;
    let mut is    = 0usize;
    for _ in 0..4 {
        let (sc0, m0) = get_scale_min_k4(is,     scales);
        let (sc1, m1) = get_scale_min_k4(is + 1, scales);
        let d1 = d * sc0;  let dm1 = dmin * m0;
        let d2 = d * sc1;  let dm2 = dmin * m1;
        for l in 0..32 {
            out[out_i] = d1 * (qs[q_i + l] & 0xF) as f32 - dm1;
            out_i += 1;
        }
        for l in 0..32 {
            out[out_i] = d2 * (qs[q_i + l] >> 4) as f32 - dm2;
            out_i += 1;
        }
        q_i += 32;
        is  += 2;
    }
}

/// Dequantize `n` elements of Q4_K data into `out`.
pub fn dequant_q4k(data: &[u8], out: &mut [f32], n: usize) {
    let n_blocks = (n + 255) / 256;
    let mut tmp = [0f32; 256];
    for b in 0..n_blocks {
        let start = b * 144;
        if start + 144 > data.len() { break; }
        dequant_q4k_block(&data[start..start + 144], &mut tmp);
        let from = b * 256;
        let to   = (from + 256).min(n);
        out[from..to].copy_from_slice(&tmp[..to - from]);
    }
}

// ── Q8_0  block = 32 weights, 34 bytes ───────────────────────────────────────
//   [0..2] d (f16 scale)
//   [2..34] qs[32] (i8 values)

pub fn dequant_q8_0(data: &[u8], out: &mut [f32], n: usize) {
    let n_blocks = (n + 31) / 32;
    for b in 0..n_blocks {
        let start = b * 34;
        if start + 34 > data.len() { break; }
        let d = f16_to_f32(u16::from_le_bytes([data[start], data[start + 1]]));
        let qs = &data[start + 2..start + 34];
        let from = b * 32;
        let to   = (from + 32).min(n);
        for i in 0..(to - from) {
            out[from + i] = d * qs[i] as i8 as f32;
        }
    }
}

// ── Q6_K  block = 256 weights, 210 bytes ─────────────────────────────────────
//   [0..128]   ql[128]  lower 4 bits
//   [128..192] qh[64]   upper 2 bits
//   [192..208] scales[16] (i8)
//   [208..210] d (f16)

pub fn dequant_q6k_block(block: &[u8], out: &mut [f32]) {
    debug_assert!(block.len() >= 210);
    let ql     = &block[0..128];
    let qh     = &block[128..192];
    let scales = &block[192..208];
    let d      = f16_to_f32(u16::from_le_bytes([block[208], block[209]]));

    for i in 0..256usize {
        let lo4  = if i % 2 == 0 { (ql[i / 2]      ) & 0xF } else { (ql[i / 2] >> 4) & 0xF };
        let hi2  = (qh[i / 4] >> ((i % 4) * 2)) & 0x3;
        let q6   = (lo4 | (hi2 << 4)) as i8 as i32 - 32;
        let sc   = scales[i / 16] as i8 as f32;
        out[i]   = d * sc * q6 as f32;
    }
}

pub fn dequant_q6k(data: &[u8], out: &mut [f32], n: usize) {
    let n_blocks = (n + 255) / 256;
    let mut tmp  = [0f32; 256];
    for b in 0..n_blocks {
        let start = b * 210;
        if start + 210 > data.len() { break; }
        dequant_q6k_block(&data[start..start + 210], &mut tmp);
        let from = b * 256;
        let to   = (from + 256).min(n);
        out[from..to].copy_from_slice(&tmp[..to - from]);
    }
}

// ── F16 row ───────────────────────────────────────────────────────────────────
pub fn dequant_f16(data: &[u8], out: &mut [f32], n: usize) {
    for i in 0..n {
        let off = i * 2;
        if off + 2 > data.len() { break; }
        out[i] = f16_to_f32(u16::from_le_bytes([data[off], data[off + 1]]));
    }
}

// ── Dispatcher ────────────────────────────────────────────────────────────────
use crate::gguf::{GGML_TYPE_F32, GGML_TYPE_F16, GGML_TYPE_Q4_0,
                  GGML_TYPE_Q8_0, GGML_TYPE_Q4_K, GGML_TYPE_Q6_K};

pub fn dequant_row(data: &[u8], out: &mut [f32], n: usize, ggml_type: u32) {
    match ggml_type {
        GGML_TYPE_F32 => {
            for i in 0..n {
                let off = i * 4;
                if off + 4 <= data.len() {
                    out[i] = f32::from_le_bytes([data[off], data[off+1], data[off+2], data[off+3]]);
                }
            }
        }
        GGML_TYPE_F16  => dequant_f16(data, out, n),
        GGML_TYPE_Q8_0 => dequant_q8_0(data, out, n),
        GGML_TYPE_Q4_K => dequant_q4k(data, out, n),
        GGML_TYPE_Q6_K => dequant_q6k(data, out, n),
        _ => { for x in &mut out[..n] { *x = 0.0; } }
    }
}

/// Dot product of f32 vector `v` with a quantized row `row_data` (n elements, given type).
/// Uses a 256-element temp buffer on the stack.
pub fn dot_quant_row(v: &[f32], row_data: &[u8], n: usize, ggml_type: u32) -> f32 {
    let mut tmp = [0f32; 256];
    let mut acc = 0f32;
    let mut off = 0usize;
    let mut rem = n;

    // Process block-by-block to avoid heap allocation
    while rem > 0 {
        let chunk = rem.min(256);
        let (block_bytes, advance) = block_range(ggml_type, off / block_elems(ggml_type), chunk);
        if off + block_bytes > row_data.len() { break; }
        dequant_row(&row_data[off..off + block_bytes], &mut tmp[..chunk], chunk, ggml_type);
        let base = n - rem;
        for i in 0..chunk {
            acc += v[base + i] * tmp[i];
        }
        off += advance;
        rem -= chunk;
    }
    acc
}

#[inline(always)]
fn block_elems(t: u32) -> usize {
    match t {
        GGML_TYPE_Q8_0 | GGML_TYPE_Q4_0 => 32,
        _ => 256,
    }
}

#[inline(always)]
fn block_range(t: u32, _block_idx: usize, _n: usize) -> (usize, usize) {
    // returns (byte_size_of_one_block, bytes_to_advance)
    match t {
        GGML_TYPE_F32  => (4, 4),
        GGML_TYPE_F16  => (2, 2),
        GGML_TYPE_Q8_0 => (34, 34),
        GGML_TYPE_Q4_K => (144, 144),
        GGML_TYPE_Q6_K => (210, 210),
        _ => (4, 4),
    }
}
