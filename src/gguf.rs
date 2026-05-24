use core::str;
use core::ptr;

pub struct GgufHeader {
    pub version:            u32,
    pub tensor_count:       u64,
    pub metadata_kv_count:  u64,
}

pub struct Gguf<'a> {
    pub data:       &'a [u8],
    pub header:     GgufHeader,
    pub meta_end:   usize,   // byte offset where tensor-info records start
    pub data_start: usize,   // byte offset where raw tensor data starts
}

#[derive(Debug, Clone, Copy)]
pub enum GgufValue<'a> {
    Uint8(u8), Int8(i8), Uint16(u16), Int16(i16),
    Uint32(u32), Int32(i32), Float32(f32),
    Uint64(u64), Int64(i64), Float64(f64),
    Bool(bool),
    String(&'a str),
    Array { value_type: u32, len: u64, data: &'a [u8] },
    Unknown,
}

// A located tensor: name, shape, GGML type id, byte offset into the tensor-data section.
#[derive(Clone, Copy)]
pub struct TensorInfo<'a> {
    pub name:        &'a str,
    pub n_dims:      u32,
    pub dims:        [u64; 4],
    pub ggml_type:   u32,
    pub data_offset: u64,   // relative to tensor-data section start
}

impl<'a> Gguf<'a> {
    pub fn new(data: &'a [u8]) -> Option<Self> {
        if data.len() < 24 { return None; }
        let ptr = data.as_ptr();
        let header = unsafe {
            if *ptr != b'G' || *ptr.add(1) != b'G' || *ptr.add(2) != b'U' || *ptr.add(3) != b'F' {
                return None;
            }
            let version           = ptr::read_unaligned(ptr.add(4)  as *const u32);
            let tensor_count      = ptr::read_unaligned(ptr.add(8)  as *const u64);
            let metadata_kv_count = ptr::read_unaligned(ptr.add(16) as *const u64);
            GgufHeader { version, tensor_count, metadata_kv_count }
        };
        let mut g = Self { data, header, meta_end: 0, data_start: 0 };
        g.meta_end   = g.metadata_end_offset();
        g.data_start = g.tensor_data_start();
        Some(g)
    }

    // ── metadata scanning ────────────────────────────────────────────────────

    pub fn find_metadata(&self, target_key: &str) -> Option<GgufValue<'a>> {
        let mut pos = 24;
        for _ in 0..self.header.metadata_kv_count {
            if pos + 8 > self.data.len() { break; }
            let key_len = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u64) } as usize;
            pos += 8;
            if pos + key_len > self.data.len() { break; }
            let key = str::from_utf8(&self.data[pos..pos + key_len]).ok()?;
            pos += key_len;
            if pos + 4 > self.data.len() { break; }
            let val_type = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u32) };
            pos += 4;
            let (val, consumed) = self.parse_value(val_type, pos)?;
            if key == target_key { return Some(val); }
            pos += consumed;
        }
        None
    }

    /// Offset where metadata ends (= start of tensor-info records).
    pub fn metadata_end_offset(&self) -> usize {
        let mut pos = 24usize;
        for _ in 0..self.header.metadata_kv_count {
            if pos + 8 > self.data.len() { break; }
            let key_len = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u64) } as usize;
            pos += 8 + key_len;
            if pos + 4 > self.data.len() { break; }
            let val_type = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u32) };
            pos += 4;
            if let Some((_, consumed)) = self.parse_value(val_type, pos) {
                pos += consumed;
            } else {
                break;
            }
        }
        pos
    }

    // ── tensor-info scanning ─────────────────────────────────────────────────

    /// Offset where the raw tensor data section starts (after header, metadata, tensor-info, alignment).
    pub fn tensor_data_start(&self) -> usize {
        let alignment: usize = 32; // GGUF default
        let mut pos = self.metadata_end_offset();
        // Skip tensor-info records
        for _ in 0..self.header.tensor_count {
            if pos + 8 > self.data.len() { break; }
            // name
            let name_len = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u64) } as usize;
            pos += 8 + name_len;
            if pos + 4 > self.data.len() { break; }
            // n_dims
            let n_dims = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u32) } as usize;
            pos += 4;
            // dims
            pos += n_dims * 8;
            // type (u32) + offset (u64)
            pos += 4 + 8;
        }
        // Align to `alignment`
        (pos + alignment - 1) & !(alignment - 1)
    }

    /// Find a tensor by name; returns its info and a byte-slice of its data.
    pub fn get_tensor(&self, target: &str) -> Option<(TensorInfo<'a>, &'a [u8])> {
        let tensor_data_off = self.data_start;
        let mut pos         = self.meta_end;

        for _ in 0..self.header.tensor_count {
            if pos + 8 > self.data.len() { break; }
            let name_len = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u64) } as usize;
            pos += 8;
            if pos + name_len > self.data.len() { break; }
            let name = str::from_utf8(&self.data[pos..pos + name_len]).unwrap_or("");
            pos += name_len;
            if pos + 4 > self.data.len() { break; }
            let n_dims = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u32) };
            pos += 4;
            let mut dims = [1u64; 4];
            for i in 0..n_dims as usize {
                if pos + 8 > self.data.len() { break; }
                dims[i] = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u64) };
                pos += 8;
            }
            if pos + 12 > self.data.len() { break; }
            let ggml_type   = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u32) };
            pos += 4;
            let data_offset = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u64) };
            pos += 8;

            let info = TensorInfo { name, n_dims, dims, ggml_type, data_offset };

            if name == target {
                // Compute number of elements
                let mut n_elems = 1usize;
                for i in 0..n_dims as usize { n_elems *= dims[i] as usize; }
                let byte_size = ggml_type_size(ggml_type, n_elems);
                let start     = tensor_data_off + data_offset as usize;
                let end       = (start + byte_size).min(self.data.len());
                if start < self.data.len() {
                    return Some((info, &self.data[start..end]));
                }
            }
        }
        None
    }

    // ── value parser ─────────────────────────────────────────────────────────

    fn parse_value(&self, val_type: u32, pos: usize) -> Option<(GgufValue<'a>, usize)> {
        if pos >= self.data.len() { return None; }
        unsafe {
            let p = self.data.as_ptr().add(pos);
            match val_type {
                0  => Some((GgufValue::Uint8(*p), 1)),
                1  => Some((GgufValue::Int8(*p as i8), 1)),
                2  => Some((GgufValue::Uint16(ptr::read_unaligned(p as *const u16)), 2)),
                3  => Some((GgufValue::Int16(ptr::read_unaligned(p as *const i16)), 2)),
                4  => Some((GgufValue::Uint32(ptr::read_unaligned(p as *const u32)), 4)),
                5  => Some((GgufValue::Int32(ptr::read_unaligned(p as *const i32)), 4)),
                6  => Some((GgufValue::Float32(ptr::read_unaligned(p as *const f32)), 4)),
                7  => Some((GgufValue::Bool(*p != 0), 1)),
                8  => {
                    if pos + 8 > self.data.len() { return None; }
                    let len = ptr::read_unaligned(p as *const u64) as usize;
                    if pos + 8 + len > self.data.len() { return None; }
                    let s = str::from_utf8(&self.data[pos + 8..pos + 8 + len]).ok()?;
                    Some((GgufValue::String(s), 8 + len))
                }
                9  => {
                    let v_type = ptr::read_unaligned(p as *const u32);
                    let len    = ptr::read_unaligned(p.add(4) as *const u64);
                    let mut ap = pos + 12;
                    for _ in 0..len {
                        let (_, consumed) = self.parse_value(v_type, ap)?;
                        ap += consumed;
                    }
                    Some((GgufValue::Array {
                        value_type: v_type,
                        len,
                        data: &self.data[pos + 12..ap],
                    }, ap - pos))
                }
                10 => Some((GgufValue::Uint64(ptr::read_unaligned(p as *const u64)), 8)),
                11 => Some((GgufValue::Int64(ptr::read_unaligned(p as *const i64)), 8)),
                12 => Some((GgufValue::Float64(ptr::read_unaligned(p as *const f64)), 8)),
                _  => None,
            }
        }
    }
}

// ── GGML type ids we handle ───────────────────────────────────────────────────
pub const GGML_TYPE_F32:   u32 = 0;
pub const GGML_TYPE_F16:   u32 = 1;
pub const GGML_TYPE_Q4_0:  u32 = 2;
pub const GGML_TYPE_Q8_0:  u32 = 8;
pub const GGML_TYPE_Q4_K:  u32 = 12; // alias used in some files; real Q4_K_S=16, Q4_K_M=17
pub const GGML_TYPE_Q4_KS: u32 = 16;
pub const GGML_TYPE_Q4_KM: u32 = 17;
pub const GGML_TYPE_Q6_K:  u32 = 20;

/// Byte size for `n_elems` elements of a GGML type.
pub fn ggml_type_size(t: u32, n: usize) -> usize {
    match t {
        GGML_TYPE_F32   => n * 4,
        GGML_TYPE_F16   => n * 2,
        GGML_TYPE_Q4_0  => { let nb = (n + 31) / 32;  nb * 18  }  // 2+16 bytes per block of 32
        GGML_TYPE_Q8_0  => { let nb = (n + 31) / 32;  nb * 34  }  // 2+32 bytes per block of 32
        GGML_TYPE_Q4_KS |
        GGML_TYPE_Q4_KM |
        GGML_TYPE_Q4_K  => { let nb = (n + 255) / 256; nb * 144 } // 144 bytes per block of 256
        GGML_TYPE_Q6_K  => { let nb = (n + 255) / 256; nb * 210 } // 210 bytes per block of 256
        _               => n * 4, // fallback F32
    }
}
