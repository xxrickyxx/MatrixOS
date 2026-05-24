extern crate alloc;
use alloc::vec::Vec;
use crate::gguf::{Gguf, GgufValue};

// ── Vocabulary reader ─────────────────────────────────────────────────────────

pub struct Tokenizer<'a> {
    tokens:     &'a [u8],   // raw GGUF array data for tokenizer.ggml.tokens
    pub count:  usize,
    pub bos:    u32,
    pub eos:    u32,
}

impl<'a> Tokenizer<'a> {
    pub fn from_gguf(gguf: &'a Gguf<'a>) -> Option<Self> {
        let tokens_array = match gguf.find_metadata("tokenizer.ggml.tokens")? {
            GgufValue::Array { value_type: 8, len, data } => (len as usize, data),
            _ => return None,
        };
        let bos = match gguf.find_metadata("tokenizer.ggml.bos_token_id") {
            Some(GgufValue::Uint32(v)) => v,
            _ => 1,
        };
        let eos = match gguf.find_metadata("tokenizer.ggml.eos_token_id") {
            Some(GgufValue::Uint32(v)) => v,
            _ => 2,
        };
        Some(Self { tokens: tokens_array.1, count: tokens_array.0, bos, eos })
    }

    /// Get the string for token `id`.
    pub fn get_token(&self, id: usize) -> Option<&'a str> {
        if id >= self.count { return None; }
        let mut pos = 0usize;
        for i in 0..=id {
            if pos + 8 > self.tokens.len() { return None; }
            let len = unsafe { *(self.tokens.as_ptr().add(pos) as *const u64) } as usize;
            pos += 8;
            if i == id {
                return core::str::from_utf8(&self.tokens[pos..pos + len]).ok();
            }
            pos += len;
        }
        None
    }

    /// Greedy BPE encode: find the longest vocabulary token matching at `text[off..]`.
    /// Returns (token_id, bytes_consumed).
    fn match_longest(&self, text: &[u8], off: usize) -> Option<(usize, usize)> {
        let mut best_id    = 0usize;
        let mut best_len   = 0usize;
        let mut pos        = 0usize;
        for id in 0..self.count {
            if pos + 8 > self.tokens.len() { break; }
            let tok_len = unsafe { *(self.tokens.as_ptr().add(pos) as *const u64) } as usize;
            pos += 8;
            let tok_start = pos;
            pos += tok_len;
            if tok_len == 0 { continue; }
            if off + tok_len > text.len() { continue; }
            if tok_len > best_len && &text[off..off + tok_len] == &self.tokens[tok_start..tok_start + tok_len] {
                best_id  = id;
                best_len = tok_len;
            }
        }
        if best_len > 0 { Some((best_id, best_len)) } else { None }
    }

    /// Encode an ASCII string into token ids (greedy longest-match).
    /// Falls back to byte-level tokens (id = byte value + offset) if no match found.
    pub fn encode(&self, text: &str, out: &mut Vec<u32>) {
        let bytes = text.as_bytes();
        let mut off = 0usize;
        while off < bytes.len() {
            if let Some((id, consumed)) = self.match_longest(bytes, off) {
                out.push(id as u32);
                off += consumed;
            } else {
                // Single byte fallback — try to find the byte as a 1-char token
                let b = bytes[off];
                // Search for single-byte token
                let mut found = false;
                let mut pos   = 0usize;
                for id in 0..self.count {
                    if pos + 8 > self.tokens.len() { break; }
                    let tl = unsafe { *(self.tokens.as_ptr().add(pos) as *const u64) } as usize;
                    pos += 8;
                    if tl == 1 && self.tokens[pos] == b {
                        out.push(id as u32);
                        found = true;
                        break;
                    }
                    pos += tl;
                }
                if !found {
                    // Last resort: use byte index directly (some tokenizers reserve [0..255] for bytes)
                    out.push(b as u32);
                }
                off += 1;
            }
        }
    }

    /// Build a prompt in Qwen2.5-Instruct ChatML format.
    pub fn build_chat_prompt(&self, user_text: &str, out: &mut Vec<u32>) {
        // <|im_start|>system\n...<|im_end|>\n<|im_start|>user\n...<|im_end|>\n<|im_start|>assistant\n
        let system = "<|im_start|>system\nYou are a helpful AI assistant integrated into MatrixOS, a bare-metal LLM operating system.<|im_end|>\n";
        let user_pre  = "<|im_start|>user\n";
        let user_post = "<|im_end|>\n<|im_start|>assistant\n";

        self.encode(system,    out);
        self.encode(user_pre,  out);
        self.encode(user_text, out);
        self.encode(user_post, out);
    }
}

// ── Token-to-text decoder ─────────────────────────────────────────────────────

/// Write the text for a token to a small fixed buffer.
/// Returns the number of bytes written.  Handles Ġ-prefixed tokens (byte-pair Ġ → space).
pub fn decode_token_to_buf(tok: &str, buf: &mut [u8]) -> usize {
    let bytes = tok.as_bytes();
    let mut i = 0usize;
    let mut o = 0usize;
    while i < bytes.len() && o < buf.len() {
        // Ġ (0xC4 0xA0) is a BPE space marker → replace with ' '
        if i + 1 < bytes.len() && bytes[i] == 0xC4 && bytes[i + 1] == 0xA0 {
            buf[o] = b' ';
            o += 1;
            i += 2;
        // Ċ (0xC4 0x8A) is BPE newline → replace with '\n'
        } else if i + 1 < bytes.len() && bytes[i] == 0xC4 && bytes[i + 1] == 0x8A {
            buf[o] = b'\n';
            o += 1;
            i += 2;
        } else if bytes[i] < 128 {
            buf[o] = bytes[i];
            o += 1;
            i += 1;
        } else {
            // multi-byte UTF-8 — skip for display purposes
            i += 1;
        }
    }
    o
}
