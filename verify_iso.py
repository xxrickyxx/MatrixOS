import struct

ISO = r"D:\MatrixOS\matrixos.iso"

with open(ISO, 'rb') as f:
    f.seek(16 * 2048)
    pvd = f.read(2048)
    assert pvd[1:6] == b'CD001', 'Not ISO9660'
    print('ISO9660 PVD: OK')

    root_lba  = struct.unpack_from('<I', pvd, 156+2)[0]
    root_size = struct.unpack_from('<I', pvd, 156+10)[0]
    print(f'Root dir: LBA={root_lba}, size={root_size}')

    f.seek(root_lba * 2048)
    dir_data = f.read(root_size)
    off = 0
    while off < root_size:
        dr_len = dir_data[off]
        if dr_len == 0:
            break
        file_lba  = struct.unpack_from('<I', dir_data, off+2)[0]
        file_size = struct.unpack_from('<I', dir_data, off+10)[0]
        name_len  = dir_data[off+32]
        name      = dir_data[off+33:off+33+name_len].decode('ascii', errors='replace')
        print(f'  {name:40s} lba={file_lba:8d} size={file_size:12,}')
        if 'MATRIXOS' in name.upper() and 'ELF' in name.upper():
            f.seek(file_lba * 2048)
            magic = f.read(4)
            status = "OK" if magic == b'\x7fELF' else "FAIL"
            print(f'    -> ELF magic: {magic.hex()} [{status}]')
        off += dr_len

print('Verification complete.')
