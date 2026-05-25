"""
Patch matrixos.elf inside matrixos.iso without rebuilding the 4.7GB image.
Only works if the new ELF is not larger than the old one.
"""
import struct, sys, os

ISO  = r"D:\MatrixOS\matrixos.iso"
ELF  = r"D:\MatrixOS\target\x86_64-matrixos\release\matrixos"
SECTOR = 2048

def read_sector(f, lba):
    f.seek(lba * SECTOR)
    return f.read(SECTOR)

def find_file_in_dir(f, dir_lba, dir_size, name_upper):
    """Walk directory records; return (file_lba, file_size, dr_offset) or None."""
    off = 0
    data = b''
    # Read all directory sectors
    sectors_needed = (dir_size + SECTOR - 1) // SECTOR
    for s in range(sectors_needed):
        data += read_sector(f, dir_lba + s)

    while off < dir_size:
        dr_len = data[off]
        if dr_len == 0:
            # Pad to next sector
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
        except:
            name = ''
        if name == name_upper:
            # Absolute byte offset of this directory record in the ISO
            sector_idx  = off // SECTOR
            within_sec  = off % SECTOR
            abs_offset  = (dir_lba + sector_idx) * SECTOR + within_sec
            return (file_lba, file_size, abs_offset)
        # Check sub-directories (bit 1 of flags = is_directory)
        if flags & 2 and name not in ('', '\x00', '\x01'):
            result = find_file_in_dir(f, file_lba, file_size, name_upper)
            if result:
                return result
        off += dr_len

    return None

def patch_iso(iso_path, elf_path):
    with open(elf_path, 'rb') as ef:
        new_elf = ef.read()
    new_size = len(new_elf)
    print(f"New ELF size: {new_size:,} bytes ({new_size // 1024 // 1024} MB)")

    with open(iso_path, 'r+b') as f:
        # Read Primary Volume Descriptor (sector 16)
        pvd = read_sector(f, 16)
        assert pvd[0] == 1 and pvd[1:6] == b'CD001', "Not a valid ISO9660 PVD"

        root_dr_offset = 156  # root directory record is at offset 156 in PVD
        root_lba  = struct.unpack_from('<I', pvd, root_dr_offset + 2)[0]
        root_size = struct.unpack_from('<I', pvd, root_dr_offset + 10)[0]
        print(f"Root dir: LBA={root_lba}, size={root_size}")

        result = find_file_in_dir(f, root_lba, root_size, 'MATRIXOS.ELF')
        if not result:
            # Try without extension variant
            result = find_file_in_dir(f, root_lba, root_size, 'MATRIXOS')
        if not result:
            print("ERROR: matrixos.elf not found in ISO directory!")
            sys.exit(1)

        file_lba, old_size, dr_abs_offset = result
        print(f"Found matrixos.elf: LBA={file_lba}, old_size={old_size:,}")

        # Max allowed: space before GGUF (LBA 18446)
        max_allowed = (18446 - file_lba) * SECTOR
        if new_size > max_allowed:
            print(f"ERROR: new ELF ({new_size:,}) > available space ({max_allowed:,}).")
            sys.exit(1)

        # Write new ELF data at the file's sector offset
        print(f"Writing new ELF at offset {file_lba * SECTOR:#x} ...")
        f.seek(file_lba * SECTOR)
        f.write(new_elf)
        # Zero out leftover bytes only if new ELF is smaller
        if new_size < old_size:
            f.write(b'\x00' * (old_size - new_size))

        # Update the directory record's data_length field (both LE and BE)
        # DR layout: off+10 = data_length LE (4 bytes), off+14 = data_length BE (4 bytes)
        print(f"Updating directory record at offset {dr_abs_offset:#x} ...")
        f.seek(dr_abs_offset + 10)
        f.write(struct.pack('<I', new_size))  # LE
        f.write(struct.pack('>I', new_size))  # BE

        print("Patch complete!")

if __name__ == '__main__':
    if not os.path.exists(ISO):
        print(f"ERROR: {ISO} not found")
        sys.exit(1)
    if not os.path.exists(ELF):
        print(f"ERROR: {ELF} not found")
        sys.exit(1)
    patch_iso(ISO, ELF)
