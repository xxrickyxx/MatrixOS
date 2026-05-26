//! Real bootloader: stage1 (MBR) → stage2 → 32-bit protected mode.
//!
//! What it does, end to end:
//!
//!   1. BIOS loads stage1 (this MBR sector) at 0:7C00 in 16-bit real mode,
//!      far-jumps to it, dl = BIOS drive number (0x80 for first HDD).
//!   2. Stage1 issues INT 13h function 0x42 (extended-read) to read N
//!      sectors of stage2 from LBA 1 into 0:7E00, then jmps to 0:7E00.
//!   3. Stage2 enables A20 via fast port 0x92, loads a flat 4-GB GDT,
//!      sets CR0.PE, far-jmps to 32-bit code in selector 0x08.
//!   4. The 32-bit handler points segment registers at the data
//!      selector 0x10, writes "MATRIXOS BOOT OK" to the VGA text buffer
//!      at 0xB8000, hlts forever.
//!
//! What it does NOT do (yet):
//!   • Long mode + paging — needs another 200 bytes; queued.
//!   • Loading the actual kernel ELF — the runtime kernel is hosted by
//!     Limine from the ISO; this bootloader is the foundation for a
//!     future "boot kernel from disk" path.
//!
//! Reading the bytes: every line of the const arrays below has a
//! mnemonic comment so you can follow `objdump -D -b binary -m i8086`
//! (or `-mi386` for stage2) output side-by-side.

// ── Layout constants ────────────────────────────────────────────────────────

/// How many 512-byte sectors stage2 occupies on disk (always written at LBA 1).
pub const STAGE2_SECTORS: u8 = 4;

/// Stage 1 — the MBR.  Exactly 512 bytes; bytes 440..456 hold our metadata
/// (overlaid by /install at the same offsets we used before), 510/511 hold
/// the BIOS boot signature.
pub const MBR_STUB: [u8; 512] = build_stage1();

/// Stage 2 — 32-bit protected-mode bring-up.  Pad to STAGE2_SECTORS × 512.
pub const STAGE2: [u8; (STAGE2_SECTORS as usize) * 512] = build_stage2();

const fn build_stage1() -> [u8; 512] {
    let mut s = [0u8; 512];
    let mut p = 0usize;

    // ── 16-bit real mode @ 0:7C00 ───────────────────────────────────────
    // cli
    s[p] = 0xFA; p += 1;
    // xor ax, ax
    s[p] = 0x31; p += 1; s[p] = 0xC0; p += 1;
    // mov ds, ax
    s[p] = 0x8E; p += 1; s[p] = 0xD8; p += 1;
    // mov es, ax
    s[p] = 0x8E; p += 1; s[p] = 0xC0; p += 1;
    // mov ss, ax
    s[p] = 0x8E; p += 1; s[p] = 0xD0; p += 1;
    // mov sp, 0x7C00
    s[p] = 0xBC; p += 1; s[p] = 0x00; p += 1; s[p] = 0x7C; p += 1;
    // sti
    s[p] = 0xFB; p += 1;
    // cld
    s[p] = 0xFC; p += 1;
    // mov [drive], dl    ; save BIOS drive number for INT 13h ext below
    //   mov byte [0x7D80], dl  →  88 16 80 7D
    s[p] = 0x88; p += 1; s[p] = 0x16; p += 1;
    s[p] = 0x80; p += 1; s[p] = 0x7D; p += 1;

    // ── Print "S1>" via INT 10h teletype so we can see we got here ──
    // mov si, msg_s1   (msg_s1 at offset 0x60 below)
    s[p] = 0xBE; p += 1; s[p] = 0x60; p += 1; s[p] = 0x7C; p += 1;
    // .print:
    //   lodsb
    let print_loop = p;
    s[p] = 0xAC; p += 1;
    //   test al, al
    s[p] = 0x84; p += 1; s[p] = 0xC0; p += 1;
    //   jz .after
    s[p] = 0x74; p += 1; s[p] = 0x05; p += 1;     // skip 5 bytes
    //   mov ah, 0x0E
    s[p] = 0xB4; p += 1; s[p] = 0x0E; p += 1;
    //   int 0x10
    s[p] = 0xCD; p += 1; s[p] = 0x10; p += 1;
    //   jmp .print
    s[p] = 0xEB; p += 1;
    let back = (print_loop as i32) - (p as i32 + 1);
    s[p] = back as i8 as u8; p += 1;
    // .after:

    // ── Read stage2 from disk via INT 13h ext-read ─────────────────────
    // Build the Disk Address Packet at ds:si = 0x7D00:
    //   db 0x10 ; size
    //   db 0    ; reserved
    //   dw N    ; sector count
    //   dw 0x7E00, 0  ; offset, segment (load address)
    //   dq 1    ; LBA start
    //
    // We write the DAP inline using mov-immediate.  Use bx=0x7D00 as
    // pointer:
    //   mov bx, 0x7D00
    s[p] = 0xBB; p += 1; s[p] = 0x00; p += 1; s[p] = 0x7D; p += 1;
    //   mov byte [bx], 0x10
    s[p] = 0xC6; p += 1; s[p] = 0x07; p += 1; s[p] = 0x10; p += 1;
    //   mov byte [bx+1], 0
    s[p] = 0xC6; p += 1; s[p] = 0x47; p += 1; s[p] = 0x01; p += 1; s[p] = 0x00; p += 1;
    //   mov word [bx+2], N (STAGE2_SECTORS)
    s[p] = 0xC7; p += 1; s[p] = 0x47; p += 1; s[p] = 0x02; p += 1;
    s[p] = STAGE2_SECTORS; p += 1; s[p] = 0x00; p += 1;
    //   mov word [bx+4], 0x7E00   (offset)
    s[p] = 0xC7; p += 1; s[p] = 0x47; p += 1; s[p] = 0x04; p += 1;
    s[p] = 0x00; p += 1; s[p] = 0x7E; p += 1;
    //   mov word [bx+6], 0        (segment)
    s[p] = 0xC7; p += 1; s[p] = 0x47; p += 1; s[p] = 0x06; p += 1;
    s[p] = 0x00; p += 1; s[p] = 0x00; p += 1;
    //   mov word [bx+8], 1        (LBA lo word)
    s[p] = 0xC7; p += 1; s[p] = 0x47; p += 1; s[p] = 0x08; p += 1;
    s[p] = 0x01; p += 1; s[p] = 0x00; p += 1;
    //   mov word [bx+10], 0
    s[p] = 0xC7; p += 1; s[p] = 0x47; p += 1; s[p] = 0x0A; p += 1;
    s[p] = 0x00; p += 1; s[p] = 0x00; p += 1;
    //   mov word [bx+12], 0
    s[p] = 0xC7; p += 1; s[p] = 0x47; p += 1; s[p] = 0x0C; p += 1;
    s[p] = 0x00; p += 1; s[p] = 0x00; p += 1;
    //   mov word [bx+14], 0
    s[p] = 0xC7; p += 1; s[p] = 0x47; p += 1; s[p] = 0x0E; p += 1;
    s[p] = 0x00; p += 1; s[p] = 0x00; p += 1;

    //   mov ah, 0x42
    s[p] = 0xB4; p += 1; s[p] = 0x42; p += 1;
    //   mov dl, [drive]    (0x7D80)
    s[p] = 0x8A; p += 1; s[p] = 0x16; p += 1; s[p] = 0x80; p += 1; s[p] = 0x7D; p += 1;
    //   mov si, bx
    s[p] = 0x89; p += 1; s[p] = 0xDE; p += 1;
    //   int 0x13
    s[p] = 0xCD; p += 1; s[p] = 0x13; p += 1;
    //   jc .read_err
    s[p] = 0x72; p += 1; s[p] = 0x08; p += 1;     // skip 8 bytes to err path
    //   jmp 0x0000:0x7E00     ; far jump to stage2
    s[p] = 0xEA; p += 1;
    s[p] = 0x00; p += 1; s[p] = 0x7E; p += 1;
    s[p] = 0x00; p += 1; s[p] = 0x00; p += 1;
    //   .read_err:
    // print 'E'
    //   mov ah, 0x0E
    s[p] = 0xB4; p += 1; s[p] = 0x0E; p += 1;
    //   mov al, 'E'
    s[p] = 0xB0; p += 1; s[p] = 0x45; p += 1;
    //   int 0x10
    s[p] = 0xCD; p += 1; s[p] = 0x10; p += 1;
    //   .hang:
    let hang = p;
    //     hlt; jmp .hang
    s[p] = 0xF4; p += 1;
    s[p] = 0xEB; p += 1;
    let back2 = (hang as i32) - (p as i32 + 1);
    s[p] = back2 as i8 as u8; p += 1;

    // Pad until offset 0x60 (96) where msg_s1 lives.
    while p < 0x60 { s[p] = 0x90; p += 1; }
    // msg_s1: "S1>", 0
    s[0x60] = b'S'; s[0x61] = b'1'; s[0x62] = b'>'; s[0x63] = 0;

    // [drive] storage at 0x7D80 = offset 0x180 — but that's outside the
    // first sector!  We deliberately use a byte INSIDE our own sector at
    // an unused slot.  The address 0x7D80 is offset 0x180 from 0x7C00 =
    // index 0x180 in our 512-byte buffer.  We zero-init it below; BIOS
    // INT 13h will write into it via the `mov [0x7D80], dl` above.
    // (Even though it'll start as 0, BIOS already passed dl=0x80 for the
    // first HDD on real hardware; if dl was something else, this saves it.)
    s[0x180] = 0;

    // Boot signature
    s[510] = 0x55;
    s[511] = 0xAA;
    s
}

const fn build_stage2() -> [u8; (STAGE2_SECTORS as usize) * 512] {
    let mut s = [0u8; (STAGE2_SECTORS as usize) * 512];
    let mut p = 0usize;

    // We are entered in real mode at 0:7E00, ds=es=ss=0, sp=0x7C00, cli is
    // already set by stage1 — wait, stage1 sets sti.  Re-disable IRQs:
    //   cli
    s[p] = 0xFA; p += 1;

    // Print "S2>" so we know stage1->stage2 chain works.
    //   mov si, msg_s2 (at offset 0x180 below = abs 0x7F80)
    s[p] = 0xBE; p += 1; s[p] = 0x80; p += 1; s[p] = 0x7F; p += 1;
    let pl = p;
    //   lodsb / test / jz +5 / mov ah,0x0E / int 0x10 / jmp pl
    s[p] = 0xAC; p += 1;
    s[p] = 0x84; p += 1; s[p] = 0xC0; p += 1;
    s[p] = 0x74; p += 1; s[p] = 0x05; p += 1;
    s[p] = 0xB4; p += 1; s[p] = 0x0E; p += 1;
    s[p] = 0xCD; p += 1; s[p] = 0x10; p += 1;
    s[p] = 0xEB; p += 1;
    let back = (pl as i32) - (p as i32 + 1);
    s[p] = back as i8 as u8; p += 1;

    // ── Fast A20 enable via port 0x92 ──────────────────────────────────
    //   in al, 0x92
    s[p] = 0xE4; p += 1; s[p] = 0x92; p += 1;
    //   or al, 2
    s[p] = 0x0C; p += 1; s[p] = 0x02; p += 1;
    //   and al, 0xFE          ; ensure reset bit clear (don't reboot!)
    s[p] = 0x24; p += 1; s[p] = 0xFE; p += 1;
    //   out 0x92, al
    s[p] = 0xE6; p += 1; s[p] = 0x92; p += 1;

    // ── Load GDT ────────────────────────────────────────────────────────
    // GDTR lives at 0x7F40 (= stage2 offset 0x140).  GDT itself starts at
    // 0x7F48 (offset 0x148).
    //   lgdt [0x7F40]
    s[p] = 0x0F; p += 1; s[p] = 0x01; p += 1; s[p] = 0x16; p += 1;
    s[p] = 0x40; p += 1; s[p] = 0x7F; p += 1;

    // ── Switch to protected mode: CR0.PE = 1 ───────────────────────────
    //   mov eax, cr0
    s[p] = 0x0F; p += 1; s[p] = 0x20; p += 1; s[p] = 0xC0; p += 1;
    //   or  eax, 1
    s[p] = 0x66; p += 1; s[p] = 0x83; p += 1; s[p] = 0xC8; p += 1; s[p] = 0x01; p += 1;
    //   mov cr0, eax
    s[p] = 0x0F; p += 1; s[p] = 0x22; p += 1; s[p] = 0xC0; p += 1;

    // ── Far jmp to 32-bit code at selector 0x08, offset = pm_entry ──
    // The label `pm_entry` is at stage2 offset 0x080 below (= linear
    // 0x7E80).  Encoded: ea <off32> <sel16>  — note in real-mode this is
    // a 32-bit operand-size override prefix + far jmp.
    //   jmp 0x08:0x7E80         (66 ea 80 7e 00 00 08 00)
    s[p] = 0x66; p += 1; s[p] = 0xEA; p += 1;
    s[p] = 0x80; p += 1; s[p] = 0x7E; p += 1; s[p] = 0x00; p += 1; s[p] = 0x00; p += 1;
    s[p] = 0x08; p += 1; s[p] = 0x00; p += 1;
    // Never reached.

    // Pad until offset 0x80 = absolute 0x7E80 (pm_entry).
    while p < 0x80 { s[p] = 0x90; p += 1; }

    // ── 32-bit protected mode entry ────────────────────────────────────
    // cs is now selector 0x08 (code).  Point ds/es/ss at selector 0x10.
    //   mov ax, 0x10
    s[p] = 0x66; p += 1; s[p] = 0xB8; p += 1;
    s[p] = 0x10; p += 1; s[p] = 0x00; p += 1; s[p] = 0x00; p += 1; s[p] = 0x00; p += 1;
    //   mov ds, ax / es, ax / ss, ax / fs, ax / gs, ax
    s[p] = 0x8E; p += 1; s[p] = 0xD8; p += 1;
    s[p] = 0x8E; p += 1; s[p] = 0xC0; p += 1;
    s[p] = 0x8E; p += 1; s[p] = 0xD0; p += 1;
    s[p] = 0x8E; p += 1; s[p] = 0xE0; p += 1;
    s[p] = 0x8E; p += 1; s[p] = 0xE8; p += 1;
    //   mov esp, 0x9000
    s[p] = 0xBC; p += 1;
    s[p] = 0x00; p += 1; s[p] = 0x90; p += 1; s[p] = 0x00; p += 1; s[p] = 0x00; p += 1;

    // ── Print "MATRIXOS BOOT OK" to VGA text mode at 0xB8000 ───────────
    //   mov edi, 0xB8000
    s[p] = 0xBF; p += 1;
    s[p] = 0x00; p += 1; s[p] = 0x80; p += 1; s[p] = 0x0B; p += 1; s[p] = 0x00; p += 1;
    //   mov esi, msg_pm    (at offset 0x1C0 below = linear 0x7FC0)
    s[p] = 0xBE; p += 1;
    s[p] = 0xC0; p += 1; s[p] = 0x7F; p += 1; s[p] = 0x00; p += 1; s[p] = 0x00; p += 1;
    //   mov ah, 0x0F        ; attribute: white on black
    s[p] = 0xB4; p += 1; s[p] = 0x0F; p += 1;
    // .copy:
    let copy = p;
    //   lodsb
    s[p] = 0xAC; p += 1;
    //   test al, al
    s[p] = 0x84; p += 1; s[p] = 0xC0; p += 1;
    //   jz .done
    s[p] = 0x74; p += 1; s[p] = 0x05; p += 1;
    //   stosw                ; write al + ah, advance edi by 2
    s[p] = 0x66; p += 1; s[p] = 0xAB; p += 1;
    //   jmp .copy
    s[p] = 0xEB; p += 1;
    let bb = (copy as i32) - (p as i32 + 1);
    s[p] = bb as i8 as u8; p += 1;
    // .done:
    //   cli
    s[p] = 0xFA; p += 1;
    let hang = p;
    //   hlt
    s[p] = 0xF4; p += 1;
    //   jmp hang
    s[p] = 0xEB; p += 1;
    let bb2 = (hang as i32) - (p as i32 + 1);
    s[p] = bb2 as i8 as u8; p += 1;

    // ── Pad to GDTR location (offset 0x140 = linear 0x7F40) ───────────
    while p < 0x140 { s[p] = 0x90; p += 1; }
    // GDTR pseudo-descriptor: limit (2 bytes) + base (4 bytes)
    // limit = 8*3 - 1 = 23
    s[p] = 23; p += 1; s[p] = 0; p += 1;
    // base = 0x7F48
    s[p] = 0x48; p += 1; s[p] = 0x7F; p += 1; s[p] = 0x00; p += 1; s[p] = 0x00; p += 1;
    // pad to 8 bytes
    s[p] = 0; p += 1; s[p] = 0; p += 1;

    // ── GDT at offset 0x148 (linear 0x7F48) ──────────────────────────
    // Entry 0: null
    p += 8;
    // Entry 1: code, selector 0x08
    //   limit_lo=0xFFFF, base_lo=0x0000, base_mid=0x00,
    //   access=0x9A (P=1 DPL=0 S=1 type=execute/read accessed=0),
    //   gran=0xCF (G=1 D/B=1 L=0 AVL=0 limit_hi=0xF), base_hi=0
    s[p+0] = 0xFF; s[p+1] = 0xFF;
    s[p+2] = 0x00; s[p+3] = 0x00; s[p+4] = 0x00;
    s[p+5] = 0x9A;
    s[p+6] = 0xCF;
    s[p+7] = 0x00;
    p += 8;
    // Entry 2: data, selector 0x10
    //   access=0x92 (read/write)
    s[p+0] = 0xFF; s[p+1] = 0xFF;
    s[p+2] = 0x00; s[p+3] = 0x00; s[p+4] = 0x00;
    s[p+5] = 0x92;
    s[p+6] = 0xCF;
    s[p+7] = 0x00;
    p += 8;

    // msg_s2 at 0x180 (linear 0x7F80): "S2>", 0
    while p < 0x180 { s[p] = 0; p += 1; }
    s[p] = b'S'; p += 1; s[p] = b'2'; p += 1; s[p] = b'>'; p += 1; s[p] = 0; p += 1;

    // msg_pm at 0x1C0 (linear 0x7FC0): "MATRIXOS BOOT OK", 0
    while p < 0x1C0 { s[p] = 0; p += 1; }
    let m = b"MATRIXOS BOOT OK";
    let mut i = 0;
    while i < m.len() { s[p] = m[i]; p += 1; i += 1; }
    s[p] = 0; p += 1;
    let _ = p;

    s
}
