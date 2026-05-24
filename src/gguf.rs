use core::str;
use core::ptr;

pub struct GgufHeader {
    pub version: u32,
    pub tensor_count: u64,
    pub metadata_kv_count: u64,
}

pub struct Gguf<'a> {
    pub data: &'a [u8],
    pub header: GgufHeader,
}

#[derive(Debug, Clone, Copy)]
pub enum GgufValue<'a> {
    Uint8(u8),
    Int8(i8),
    Uint16(u16),
    Int16(i16),
    Uint32(u32),
    Int32(i32),
    Float32(f32),
    Uint64(u64),
    Int64(i64),
    Float64(f64),
    Bool(bool),
    String(&'a str),
    Array { value_type: u32, len: u64, data: &'a [u8] },
    Unknown,
}

impl<'a> Gguf<'a> {
    pub fn new(data: &'a [u8]) -> Option<Self> {
        if data.len() < 24 { return None; }
        let ptr = data.as_ptr();
        unsafe {
            if *ptr != b'G' || *ptr.add(1) != b'G' || *ptr.add(2) != b'U' || *ptr.add(3) != b'F' {
                return None;
            }
            let version = ptr::read_unaligned(ptr.add(4) as *const u32);
            let tensor_count = ptr::read_unaligned(ptr.add(8) as *const u64);
            let metadata_kv_count = ptr::read_unaligned(ptr.add(16) as *const u64);
            Some(Self {
                data,
                header: GgufHeader { version, tensor_count, metadata_kv_count },
            })
        }
    }

    pub fn find_metadata(&self, target_key: &str) -> Option<GgufValue<'a>> {
        let mut pos = 24;
        for _ in 0..self.header.metadata_kv_count {
            if pos + 8 > self.data.len() { break; }
            let key_len = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u64) } as usize;
            pos += 8;
            if pos + key_len > self.data.len() { break; }
            let key = str::from_utf8(&self.data[pos..pos+key_len]).ok()?;
            pos += key_len;
            
            if pos + 4 > self.data.len() { break; }
            let val_type = unsafe { ptr::read_unaligned(self.data.as_ptr().add(pos) as *const u32) };
            pos += 4;
            
            let (val, consumed) = self.parse_value(val_type, pos)?;
            if key == target_key {
                return Some(val);
            }
            pos += consumed;
        }
        None
    }

    fn parse_value(&self, val_type: u32, pos: usize) -> Option<(GgufValue<'a>, usize)> {
        if pos >= self.data.len() { return None; }
        unsafe {
            let p = self.data.as_ptr().add(pos);
            match val_type {
                0 => Some((GgufValue::Uint8(*p), 1)),
                1 => Some((GgufValue::Int8(*p as i8), 1)),
                2 => Some((GgufValue::Uint16(ptr::read_unaligned(p as *const u16)), 2)),
                3 => Some((GgufValue::Int16(ptr::read_unaligned(p as *const i16)), 2)),
                4 => Some((GgufValue::Uint32(ptr::read_unaligned(p as *const u32)), 4)),
                5 => Some((GgufValue::Int32(ptr::read_unaligned(p as *const i32)), 4)),
                6 => Some((GgufValue::Float32(ptr::read_unaligned(p as *const f32)), 4)),
                7 => Some((GgufValue::Bool(*p != 0), 1)),
                8 => {
                    let len = ptr::read_unaligned(p as *const u64) as usize;
                    let s = str::from_utf8(&self.data[pos+8..pos+8+len]).ok()?;
                    Some((GgufValue::String(s), 8 + len))
                }
                9 => {
                    let v_type = ptr::read_unaligned(p as *const u32);
                    let len = ptr::read_unaligned(p.add(4) as *const u64);
                    let mut array_pos = pos + 12;
                    for _ in 0..len {
                        let (_, consumed) = self.parse_value(v_type, array_pos)?;
                        array_pos += consumed;
                    }
                    Some((GgufValue::Array { value_type: v_type, len, data: &self.data[pos+12..array_pos] }, array_pos - pos))
                }
                10 => Some((GgufValue::Uint64(ptr::read_unaligned(p as *const u64)), 8)),
                11 => Some((GgufValue::Int64(ptr::read_unaligned(p as *const i64)), 8)),
                12 => Some((GgufValue::Float64(ptr::read_unaligned(p as *const f64)), 8)),
                _ => None,
            }
        }
    }
}
