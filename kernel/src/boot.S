// milkOS entry point. qemu -kernel jumps here at EL1 with the MMU off, at the physical
// load address (0x40080000). The kernel is linked at KERNEL_BASE + that (the higher half,
// TTBR1), so until the MMU is on and we have branched up there, everything here must be
// PC-relative (adrp/adr, b/bl): no `ldr =symbol` (that's the high address) except the
// jump itself. Included by mmu.rs via global_asm! with the constants it needs.
.section .text.boot, "ax"
.global _start
_start:
    // arm64 Linux "Image" header (64 bytes). It makes qemu treat a flat binary of
    // this kernel as a Linux-style image, which is what gets us a device tree
    // pointer in x0 (qemu passes none for a plain ELF). The ELF entry is still
    // _start, so this first instruction just branches over the header.
    b       _real_start
    .long   0
    .quad   0x80000                         // text_offset from start of RAM
    .quad   __image_size                    // size incl. .bss and stack (linker.ld)
    .quad   0x2                             // flags: little-endian, 4K pages
    .quad   0
    .quad   0
    .quad   0
    .long   0x644d5241                      // magic "ARM\x64"
    .long   0
_real_start:
    // qemu hands us the device tree pointer in x0. Keep it in x19 (nothing below
    // touches x19) until we run high, then store it in __boot_dtb.
    mov     x19, x0

    // Only core 0 runs; the others are started later via PSCI (smp.S).
    mrs     x0, mpidr_el1
    and     x0, x0, #0xff
    cbnz    x0, 2f

    // Zero .bss (physical addresses: MMU off). The boot tables live in it.
    adrp    x0, __bss_start
    add     x0, x0, :lo12:__bss_start
    adrp    x1, __bss_end
    add     x1, x1, :lo12:__bss_end
1:  cmp     x0, x1
    b.hs    3f
    str     xzr, [x0], #8
    b       1b
3:
    // Boot tables: one L0 + one L1, used for both TTBR0 (identity, so the next few
    // instructions keep running) and TTBR1 (KERNEL_BASE + pa: index bits 47:39 are the
    // same). L1[0] = 1 GiB of MMIO, L1[1] = the first 1 GiB of RAM (the image, its
    // stack, the DTB). mmu::init replaces TTBR1 with the full linear map.
    adrp    x0, boot_l0
    adrp    x1, boot_l1
    orr     x2, x1, #3                      // table descriptor
    str     x2, [x0]
    ldr     x2, ={BLOCK_DEV}
    str     x2, [x1]
    ldr     x2, ={BLOCK_RAM}
    mov     x3, #0x40000000
    orr     x2, x2, x3
    str     x2, [x1, #8]
    dsb     sy

    ldr     x2, ={MAIR}
    msr     mair_el1, x2
    // TCR: both halves 48-bit, 4K, WBWA, inner shareable; IPS = the CPU's PA size,
    // capped at 48 bits (0b101).
    ldr     x2, ={TCR}
    mrs     x3, id_aa64mmfr0_el1
    and     x3, x3, #0xf
    mov     x4, #5
    cmp     x3, x4
    csel    x3, x3, x4, ls
    orr     x2, x2, x3, lsl #32
    msr     tcr_el1, x2
    msr     ttbr0_el1, x0
    msr     ttbr1_el1, x0
    isb
    tlbi    vmalle1
    ic      iallu
    dsb     nsh
    isb
    mrs     x2, sctlr_el1
    ldr     x3, ={SCTLR_ON}
    orr     x2, x2, x3
    ldr     x3, ={SCTLR_OFF}
    bic     x2, x2, x3
    dsb     sy
    msr     sctlr_el1, x2
    isb

    // Up to the higher half.
    ldr     x2, =4f
    br      x2
4:  ldr     x0, =__stack_top
    mov     sp, x0
    ldr     x0, =__boot_dtb
    str     x19, [x0]
    msr     tpidr_el1, xzr                  // per-CPU index 0 (percpu.rs)
    mov     x29, xzr
    mov     x30, xzr
    bl      kmain
2:  wfe
    b       2b
.ltorg

// Device tree pointer from the bootloader (0 if none). Lives in .data, not .bss,
// so it is a plain initialised word. Read it with mem::boot_dtb().
.section .data
.balign 8
.global __boot_dtb
__boot_dtb:
    .quad 0

// The boot page tables (zeroed with the rest of .bss before use). boot_l0 is kept as
// the identity map secondaries turn their MMU on through (mmu::boot_ttbr0).
.section .bss.boot_pt, "aw", @nobits
.balign 4096
.global boot_l0
boot_l0:
    .space 4096
boot_l1:
    .space 4096
