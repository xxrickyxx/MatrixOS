"""
Patch model.gguf inside matrixos.iso without rebuilding the image.
Only works if the new GGUF is not larger than the original ISO trailing area.

Usage:  python patch_model.py path/to/new_model.gguf
"""
import struct, sys, os

ISO    = r"D:\MatrixOS\matrixos.iso"
SECTOR = 2048

def read_sector(f, lba):
    f.seek(lba * SECTOR)
    return f.read(SECTOR)

def find_file_in_dir(f, dir_lba, dir_size, name_upper):
    """Walk directory records; return (file_lba, file_size, dr_offset) or None."""
    off  = 0
    data = b''
    sectors_needed = (dir_size + SECTOR - 1) // SECTOR
    for s in range(sectors_needed):
        data += read_sector(f, dir_lba + s)

    while off < dir_size:
        dr_len = data[off]
        if dr_len == 0:
            off = (off // SECTOR + 1) * SECTOR
            continue
        if off + dr_len > len(data):
            break
        flags      = data[off + 25]
        file_lba   = struct.unpack_from('<I', data, off + 2)[0]
        file_size  = struct.unpack_from('<I', data, off + 10)[0]
        name_len   = data[off + 32]
        name_bytes = data[off + 33: off + 33 + name_len]
        try:
            name = name_bytes.decode('ascii', errors='replace').split(';')[0].upper()
        except Exception:
            name = ''
        if name == name_upper:
            sector_idx = off // SECTOR
            within_sec = off % SECTOR
            abs_offset = (dir_lba + sector_idx) * SECTOR + within_sec
            return (file_lba, file_size, abs_offset)
        if flags & 2 and name not in ('', '\x00', '\x01'):
            r = find_file_in_dir(f, file_lba, file_size, name_upper)
            if r:
                return r
        off += dr_len
    return None

def patch_model(iso_path, gguf_path):
    new_size = os.path.getsize(gguf_path)
    iso_size = os.path.getsize(iso_path)
    print(f"New GGUF size: {new_size:,} bytes ({new_size / 1024 / 1024:.1f} MB)")
    print(f"ISO total:     {iso_size:,} bytes")

    with open(iso_path, 'r+b') as f:
        pvd = read_sector(f, 16)
        assert pvd[0] == 1 and pvd[1:6] == b'CD001', "Not a valid ISO9660 PVD"

        root_lba  = struct.unpack_from('<I', pvd, 156 + 2)[0]
        root_size = struct.unpack_from('<I', pvd, 156 + 10)[0]

        result = find_file_in_dir(f, root_lba, root_size, 'MODEL.GGUF')
        if not result:
            result = find_file_in_dir(f, root_lba, root_size, 'MODEL')
        if not result:
            print("ERROR: model.gguf not found in ISO!")
            sys.exit(1)

        file_lba, old_size, dr_abs_offset = result
        print(f"Found model.gguf: LBA={file_lba}, old_size={old_size:,} bytes")

        max_allowed = iso_size - (file_lba * SECTOR)
        if new_size > max_allowed:
            print(f"ERROR: new GGUF ({new_size:,}) > available trailing space ({max_allowed:,}).")
            sys.exit(1)

        print(f"Writing new GGUF at offset {file_lba * SECTOR:#x} ...")
        f.seek(file_lba * SECTOR)
        # Stream the file in 8MB chunks to avoid loading the whole thing in RAM
        with open(gguf_path, 'rb') as gf:
            written = 0
            chunk_size = 8 * 1024 * 1024
            while True:
                buf = gf.read(chunk_size)
                if not buf:
                    break
                f.write(buf)
                written += len(buf)
                pct = written * 100 // new_size if new_size else 100
                sys.stdout.write(f"\r  ... {written/1024/1024:.0f} MB ({pct}%)")
                sys.stdout.flush()
        print()

        # Update DR size (LE then BE)
        print(f"Updating directory record at offset {dr_abs_offset:#x} ...")
        f.seek(dr_abs_offset + 10)
        f.write(struct.pack('<I', new_size))
        f.write(struct.pack('>I', new_size))

        print("Model patch complete!")

if __name__ == '__main__':
    if len(sys.argv) != 2:
        print("Usage: python patch_model.py <new_model.gguf>")
        sys.exit(1)
    gguf = sys.argv[1]
    if not os.path.exists(ISO):
        print(f"ERROR: {ISO} not found"); sys.exit(1)
    if not os.path.exists(gguf):
        print(f"ERROR: {gguf} not found"); sys.exit(1)
    patch_model(ISO, gguf)
