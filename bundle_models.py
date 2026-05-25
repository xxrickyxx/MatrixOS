"""
Concatenate two GGUF models into a single bundle file that MatrixOS
can load as one Limine module.  The kernel detects the "MTXBNDLE" magic
and exposes both models for runtime switching.

Layout:
    bytes  0.. 8   magic "MTXBNDLE"
    bytes  8..12   u32 version (=1)
    bytes 12..16   u32 num_models (=2)
    bytes 16..24   u64 model_a_offset    (absolute, into the bundle)
    bytes 24..32   u64 model_a_size      (bytes)
    bytes 32..40   u64 model_b_offset
    bytes 40..48   u64 model_b_size
    bytes 48..64   reserved (zero)
    bytes 64..N    name_a (null-terminated, up to 32 bytes)
    bytes N..M    name_b (null-terminated, up to 32 bytes)
    padding to next 4096-byte boundary
    [model A GGUF bytes]
    padding to next 4096-byte boundary
    [model B GGUF bytes]
"""
import struct, sys, os

ALIGN = 4096

def align_up(n, a):
    return (n + a - 1) & ~(a - 1)

def bundle(model_a_path, name_a, model_b_path, name_b, out_path):
    size_a = os.path.getsize(model_a_path)
    size_b = os.path.getsize(model_b_path)
    # Header is 64 bytes fixed + up to 64 bytes names → keep simple, 128 bytes
    header_total = 128
    a_off = align_up(header_total, ALIGN)
    b_off = align_up(a_off + size_a, ALIGN)
    total = b_off + size_b
    print(f"Bundle layout:")
    print(f"  model A '{name_a}': offset {a_off:#x}, size {size_a:,}")
    print(f"  model B '{name_b}': offset {b_off:#x}, size {size_b:,}")
    print(f"  total bundle: {total:,} bytes ({total/1024/1024:.1f} MB)")
    with open(out_path, 'wb') as out:
        # 64-byte fixed header
        hdr = bytearray(64)
        hdr[0:8] = b'MTXBNDLE'
        struct.pack_into('<I', hdr,  8, 1)        # version
        struct.pack_into('<I', hdr, 12, 2)        # num_models
        struct.pack_into('<Q', hdr, 16, a_off)
        struct.pack_into('<Q', hdr, 24, size_a)
        struct.pack_into('<Q', hdr, 32, b_off)
        struct.pack_into('<Q', hdr, 40, size_b)
        out.write(bytes(hdr))
        # 64-byte names (32 each)
        n_a = name_a.encode('ascii')[:31] + b'\x00'
        n_a = n_a + b'\x00' * (32 - len(n_a))
        n_b = name_b.encode('ascii')[:31] + b'\x00'
        n_b = n_b + b'\x00' * (32 - len(n_b))
        out.write(n_a + n_b)
        # Pad to a_off
        out.write(b'\x00' * (a_off - out.tell()))
        # Model A
        print("  copying model A...")
        with open(model_a_path, 'rb') as f:
            while True:
                buf = f.read(8 * 1024 * 1024)
                if not buf: break
                out.write(buf)
        # Pad to b_off
        out.write(b'\x00' * (b_off - out.tell()))
        # Model B
        print("  copying model B...")
        with open(model_b_path, 'rb') as f:
            while True:
                buf = f.read(8 * 1024 * 1024)
                if not buf: break
                out.write(buf)
    actual = os.path.getsize(out_path)
    print(f"Bundle written: {actual:,} bytes")

if __name__ == '__main__':
    if len(sys.argv) != 6:
        print("Usage: python bundle_models.py <model_a.gguf> <name_a> <model_b.gguf> <name_b> <out.bundle>")
        sys.exit(1)
    bundle(sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5])
