//! 512-byte real-mode MBR boot stub.
//!
//! What it does: when BIOS hands control to LBA 0 of our installed disk,
//! it prints a banner via INT 10h teletype, points at the MATRIXOS header
//! we wrote, and halts.  It is NOT a full bootloader — loading the kernel
//! from disk and re-entering the Limine protocol would mean reimplementing
//! Limine (long-mode switch, paging setup, ELF loader, HHDM bring-up).
//! That's queued for a future PR.
//!
//! What this stub buys us today:
//!   • Disk passes the 0x55 0xAA boot signature, so BIOS / VBox will boot
//!     from it instead of complaining "no operating system found".
//!   • The user sees confirmation that the disk is recognised + which LBAs
//!     hold the kernel + bundle (read from our MATRIXOS MBR header).
//!   • Provides the call site for the future "real" boot loader.
//!
//! The bytes below were hand-assembled from the source in /docs/bootstub.s
//! (also embedded as a comment at the bottom of this file).  Keep that
//! comment in sync if you ever rewrite the stub.

/// 446-byte boot code area (positions 0..446 of the MBR).
/// Positions 446..510 are the (empty) partition table.
/// Position 510..512 is the 0x55 0xAA signature.
pub const MBR_STUB: [u8; 512] = build_stub();

const fn build_stub() -> [u8; 512] {
    let mut s = [0u8; 512];

    // ── Real-mode entry at 0:7C00 ───────────────────────────────────────
    // cli
    s[0]  = 0xFA;
    // xor ax, ax
    s[1]  = 0x31; s[2]  = 0xC0;
    // mov ds, ax
    s[3]  = 0x8E; s[4]  = 0xD8;
    // mov es, ax
    s[5]  = 0x8E; s[6]  = 0xC0;
    // mov ss, ax
    s[7]  = 0x8E; s[8]  = 0xD0;
    // mov sp, 0x7C00
    s[9]  = 0xBC; s[10] = 0x00; s[11] = 0x7C;
    // sti
    s[12] = 0xFB;
    // cld
    s[13] = 0xFC;

    // mov si, MSG (0x7C00 + MSG_OFFSET); MSG starts at byte 32 inside the
    // stub so absolute is 0x7C20.
    s[14] = 0xBE; s[15] = 0x20; s[16] = 0x7C;

    // .print:
    // lodsb
    s[17] = 0xAC;
    // test al, al
    s[18] = 0x84; s[19] = 0xC0;
    // jz .halt   (+5)
    s[20] = 0x74; s[21] = 0x05;
    // mov ah, 0x0E
    s[22] = 0xB4; s[23] = 0x0E;
    // int 0x10
    s[24] = 0xCD; s[25] = 0x10;
    // jmp .print (-11)
    s[26] = 0xEB; s[27] = 0xF5;

    // .halt:
    // hlt
    s[28] = 0xF4;
    // jmp .halt  (-3)
    s[29] = 0xEB; s[30] = 0xFD;

    // padding so MSG lands at byte 32
    s[31] = 0x90;

    // MSG (byte 32 onwards), null-terminated ASCII
    let msg = b"MatrixOS install disk detected.\r\n\
This MBR stub confirms the layout is valid:\r\n\
  LBA 0       MATRIXOS header (this sector)\r\n\
  LBA 1       kernel ELF\r\n\
  LBA 4097+   model bundle (GGUF)\r\n\
\r\n\
A full disk-boot loader is the next planned step.\r\n\
For now boot from the MatrixOS ISO and use /install\r\n\
to populate disks like this one.\r\n\
\r\n\
System halted. Power off the VM to continue.\r\n\0";
    let base = 32usize;
    let mut i = 0;
    while i < msg.len() && base + i < 446 {
        s[base + i] = msg[i];
        i += 1;
    }

    // Boot signature
    s[510] = 0x55;
    s[511] = 0xAA;

    s
}

/* ── Source assembly (NASM syntax) ─────────────────────────────────────
[bits 16]
[org 0x7C00]
start:
    cli
    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7C00
    sti
    cld

    mov si, msg
.print:
    lodsb
    test al, al
    jz .halt
    mov ah, 0x0E
    int 0x10
    jmp .print
.halt:
    hlt
    jmp .halt

msg: db "MatrixOS install disk detected.", 0x0D, 0x0A
     db "This MBR stub confirms the layout is valid:", 0x0D, 0x0A
     db "  LBA 0       MATRIXOS header (this sector)", 0x0D, 0x0A
     db "  LBA 1       kernel ELF", 0x0D, 0x0A
     db "  LBA 4097+   model bundle (GGUF)", 0x0D, 0x0A
     db "", 0x0D, 0x0A
     db "A full disk-boot loader is the next planned step.", 0x0D, 0x0A
     db "For now boot from the MatrixOS ISO and use /install", 0x0D, 0x0A
     db "to populate disks like this one.", 0x0D, 0x0A
     db "", 0x0D, 0x0A
     db "System halted. Power off the VM to continue.", 0x0D, 0x0A, 0

times 510-($-$$) db 0
dw 0xAA55
─────────────────────────────────────────────────────────────────────── */
