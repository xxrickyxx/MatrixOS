"""
Create a minimal valid GGUF file for fast boot testing.
Uses tiny model dims so it fits in ~16MB heap and boots instantly.
"""
import struct, os

def pack_str(s: str) -> bytes:
    b = s.encode('utf-8')
    return struct.pack('<Q', len(b)) + b

def pack_kv(key: str, val_type: int, val_bytes: bytes) -> bytes:
    return pack_str(key) + struct.pack('<I', val_type) + val_bytes

def u32(n): return struct.pack('<I', n)
def u64(n): return struct.pack('<Q', n)
def f32(f): return struct.pack('<f', f)

# ── Metadata ─────────────────────────────────────────────────────────────────
kvs = []
# general
kvs.append(pack_kv("general.name",             8, pack_str("MatrixOS-Test-1M")))
kvs.append(pack_kv("general.architecture",     8, pack_str("llama")))
# model dims (tiny: d=64, layers=2, heads=4, kv_heads=1, ffn=128, vocab=256)
kvs.append(pack_kv("llama.embedding_length",        4, u32(64)))
kvs.append(pack_kv("llama.block_count",             4, u32(2)))
kvs.append(pack_kv("llama.attention.head_count",    4, u32(4)))
kvs.append(pack_kv("llama.attention.head_count_kv", 4, u32(1)))
kvs.append(pack_kv("llama.feed_forward_length",     4, u32(128)))
kvs.append(pack_kv("llama.vocab_size",              4, u32(256))))
kvs.append(pack_kv("llama.rope.freq_base",          6, f32(10000.0)))
kvs.append(pack_kv("llama.context_length",          4, u32(512)))
# tokenizer
kvs.append(pack_kv("tokenizer.ggml.model",          8, pack_str("gpt2")))
kvs.append(pack_kv("tokenizer.ggml.bos_token_id",   4, u32(1)))
kvs.append(pack_kv("tokenizer.ggml.eos_token_id",   4, u32(2)))

# Build token vocabulary: 256 single-char tokens
# Type 9 = array of strings
token_list = b''
token_count = 0
specials = {0: "<unk>", 1: "<s>", 2: "</s>"}
for i in range(256):
    tok = specials.get(i, chr(i) if 32 <= i < 127 else f"<0x{i:02X}>")
    tok_b = tok.encode('utf-8')
    token_list += struct.pack('<Q', len(tok_b)) + tok_b
    token_count += 1

# array header: val_type=8 (string), count=256
arr_header = struct.pack('<I', 8) + struct.pack('<Q', token_count)
kvs.append(pack_kv("tokenizer.ggml.tokens", 9, arr_header + token_list))

metadata_bytes = b''.join(kvs)
n_tensors = 0
n_metadata = len(kvs)

# ── GGUF file ──────────────────────────────────────────────────────────────────
out = b'GGUF'
out += struct.pack('<I', 3)           # version
out += struct.pack('<Q', n_tensors)   # tensor count (0 = no weights)
out += struct.pack('<Q', n_metadata)  # metadata count
out += metadata_bytes

path = r"D:\MatrixOS\test_model.gguf"
with open(path, 'wb') as f:
    f.write(out)

print(f"Written {len(out):,} bytes to {path}")
