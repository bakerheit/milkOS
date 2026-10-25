// milkOS entry point. The bootloader jumps here with the MMU off, at the physical load
// address: qemu virt at 0x40080000 (EL1), the Raspberry Pi firmware and qemu raspi4b at
// 0x80000 (EL2; el.S drops to EL1). The kernel is linked at KERNEL_BASE + that (the higher
// half, TTBR1), so until the MMU is on and we have branched up there, everything here must
// be PC-relative (adrp/adr, b/bl): no `ldr =symbol` (that's the high address) except the
// jump itself. Included by mmu.rs via global_asm! (behind el.S) with the constants it needs.
.section .text.boot, "ax"
.global _start
_start:
    // arm64 Linux "Image" header (64 bytes). It makes qemu treat a flat binary of
    // this kernel as a Linux-style image, which is what gets us a device tree
    // pointer in x0 (qemu passes none for a plain ELF). The ELF entry is still
    // _start, so this first instruction just branches over the header.
    b       _real_start
    .long   0
    .quad   {TEXT_OFFSET}                   // text_offset from start of RAM (0x80000)
    .quad   __image_size                    // size incl. .bss and stack (linker.ld)
    .quad   0x2                             // flags: little-endian, 4K pages
    .quad   0
    .quad   0
    .quad   0
    .long   0x644d5241                      // magic "ARM\x64"
    .long   0
_real_start:
    // The bootloader hands us the device tree pointer in x0. Keep it in x19 (nothing
    // below touches x19) until we run high, then store it in __boot_dtb.
    mov     x19, x0
    mrs     x20, CurrentEL                  // the exception level we were entered at (logged by rpi::diag)

    // Only core 0 runs; the others are started later (PSCI or the spin table, smp.rs).
    mrs     x0, mpidr_el1
    and     x0, x0, #0xff
    cbnz    x0, 2f

    // EL2 -> EL1 if the firmware started us at EL2 (el.S).
    drop_to_el1 x0

.if {RELOC}
    // The Pixel 2: the Android bootloader loads the kernel at an address of its own choosing (its
    // rules for an arm64 Image are not known for sure), which need not be the one this image is
    // linked for. Entered anywhere else: move the image there, then carry on from the same
    // place in the copy. Named labels only: the numeric ones below belong to the code after it.
    adr     x0, _start                      // L: where we were loaded
    ldr     x1, =__load_phys                // K: where we are linked to run
    adrp    x9, __boot_entry
    add     x9, x9, :lo12:__boot_entry
    str     x0, [x9]                        // for the log (RAM, MMU off: a plain store)
    cmp     x0, x1
    b.eq    .Lreloc_done
    ldr     x2, =__copy_len                 // bytes of the image the loader brought
    ldr     x3, =__image_size               // bytes it needs in memory (.bss and the stack too)
    mov     x4, #0                          // size of the device tree, 0 if x19 is not one
    cbz     x19, .Lreloc_ends
    ldr     w5, [x19]
    rev     w5, w5
    movz    w6, #0xfeed
    movk    w6, #0xd00d, lsl #16
    cmp     w5, w6
    b.ne    .Lreloc_ends
    ldr     w4, [x19, #4]
    rev     w4, w4
    add     w4, w4, #7
    and     w4, w4, #-8
.Lreloc_ends:
    // x5 = the end of everything involved, rounded up to 64 KiB, plus 64 KiB: the stub goes there,
    // the tree's copy 4 KiB behind it.
    add     x5, x0, x2
    add     x6, x1, x3
    cmp     x5, x6
    csel    x5, x6, x5, lo
    cbz     x4, .Lreloc_notree
    add     x6, x19, x4
    cmp     x5, x6
    csel    x5, x6, x5, lo
.Lreloc_notree:
    mov     x7, #0xffff
    add     x5, x5, x7
    bic     x5, x5, x7
    add     x5, x5, #0x10, lsl #12
    add     x6, x5, #0x1000                 // where the tree is copied to
    // Lines of these ranges may be dirty in the data cache: write them back and drop them before the
    // copy (done with the MMU off, straight to memory) so no stale line is written over it later.
    mrs     x7, ctr_el0
    ubfx    x7, x7, #16, #4
    mov     x8, #4
    lsl     x7, x8, x7                      // data cache line size
    sub     x8, x7, #1
    bic     x9, x0, x8                      // the source image
    add     x10, x0, x2
.Lreloc_c1:
    dc      civac, x9
    add     x9, x9, x7
    cmp     x9, x10
    b.lo    .Lreloc_c1
    bic     x9, x1, x8                      // the destination image
    add     x10, x1, x3
.Lreloc_c2:
    dc      civac, x9
    add     x9, x9, x7
    cmp     x9, x10
    b.lo    .Lreloc_c2
    cbz     x4, .Lreloc_c4                  // the tree
    bic     x9, x19, x8
    add     x10, x19, x4
.Lreloc_c3:
    dc      civac, x9
    add     x9, x9, x7
    cmp     x9, x10
    b.lo    .Lreloc_c3
.Lreloc_c4:
    bic     x9, x5, x8                      // the stub and the tree's new place
    add     x10, x6, x4
    add     x10, x10, #0x1000
.Lreloc_c5:
    dc      civac, x9
    add     x9, x9, x7
    cmp     x9, x10
    b.lo    .Lreloc_c5
    dsb     sy
    adr     x9, .Lreloc_stub                // copy the stub to its place and go there
    adr     x10, .Lreloc_stub_end
    mov     x11, x5
.Lreloc_c6:
    ldr     x12, [x9], #8
    str     x12, [x11], #8
    cmp     x9, x10
    b.lo    .Lreloc_c6
    dsb     sy
    ic      iallu
    dsb     sy
    isb
    br      x5

    .balign 16
.Lreloc_stub:                               // runs at x5, position independent
    // x0 = L, x1 = K, x2 = bytes to move, x4 = tree size (0: none), x6 = where the tree goes, x19 = tree
    cbz     x4, .Lstub_image
    mov     x7, x19
    mov     x8, x6
    add     x9, x19, x4
.Lstub_t:
    ldr     x10, [x7], #8
    str     x10, [x8], #8
    cmp     x7, x9
    b.lo    .Lstub_t
    mov     x19, x6
.Lstub_image:
    add     x2, x2, #7
    bic     x2, x2, #7
    cmp     x1, x0
    b.hi    .Lstub_down
    mov     x7, x0                          // K <= L: copy upwards
    mov     x8, x1
    add     x9, x0, x2
.Lstub_up:
    ldr     x10, [x7], #8
    str     x10, [x8], #8
    cmp     x7, x9
    b.lo    .Lstub_up
    b       .Lstub_copied
.Lstub_down:                                // K > L: copy downwards (the ranges may overlap)
    add     x7, x0, x2
    add     x8, x1, x2
.Lstub_dn:
    ldr     x10, [x7, #-8]!
    str     x10, [x8, #-8]!
    cmp     x7, x0
    b.hi    .Lstub_dn
.Lstub_copied:
    dsb     sy
    ic      iallu
    dsb     sy
    isb
    add     x7, x1, #(.Lreloc_done - _start)
    br      x7
.Lreloc_stub_end:
.Lreloc_done:
    // here in the copy (or where we started, if that was the right place); x19 = the tree, x20 = the EL
.endif

    // Whoever loaded the image may have left lines of it (or stale ones) in the data cache: clean and
    // invalidate the whole image to the point of coherency before anything reads it with caches on.
    // (The Raspberry Pi firmware copies the kernel in with its own agents; harmless on qemu.)
    mrs     x2, ctr_el0
    ubfx    x2, x2, #16, #4
    mov     x3, #4
    lsl     x2, x3, x2                      // data cache line size in bytes
    adr     x0, _start
    ldr     x1, =__image_size
    add     x1, x0, x1
    sub     x3, x2, #1
    bic     x0, x0, x3
1:  dc      civac, x0
    add     x0, x0, x2
    cmp     x0, x1
    b.lo    1b
    dsb     sy
    isb

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
    // same). The four L1 entries (1 GiB each) are the board's (platform.rs boot_map): on
    // virt MMIO then RAM (the image, its stack, the DTB), on the Pi RAM, nothing, nothing,
    // MMIO. mmu::init replaces TTBR1 with the full linear map.
    adrp    x0, boot_l0
    adrp    x1, boot_l1
    orr     x2, x1, #3                      // table descriptor
    str     x2, [x0]
    ldr     x2, ={L1_0}
    str     x2, [x1]
    ldr     x2, ={L1_1}
    str     x2, [x1, #8]
    ldr     x2, ={L1_2}
    str     x2, [x1, #16]
    ldr     x2, ={L1_3}
    str     x2, [x1, #24]
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
    ldr     x0, =__boot_el
    str     x20, [x0]
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
// Where the loader put the image (the address boot.S was entered at; only the Pixel 2 build records it).
.global __boot_entry
__boot_entry:
    .quad 0
// CurrentEL as the kernel was entered (the value of the register: EL << 2).
.global __boot_el
__boot_el:
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

.purgem drop_to_el1
