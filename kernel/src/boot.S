// milkOS entry point. qemu -kernel jumps here at EL1 with the MMU off.
.section .text.boot
.global _start
_start:
    // arm64 Linux "Image" header (64 bytes). It makes qemu treat a flat binary of
    // this kernel as a Linux-style image, which is what gets us a device tree
    // pointer in x0 (qemu passes none for a plain ELF). The ELF entry is still
    // _start, so this first instruction just branches over the header.
    b       _real_start
    .long   0
    .quad   0x80000                         // text_offset from start of RAM
    .quad   __image_size                      // size incl. .bss and stack (linker.ld)
    .quad   0x2                             // flags: little-endian, 4K pages
    .quad   0
    .quad   0
    .quad   0
    .long   0x644d5241                      // magic "ARM\x64"
    .long   0
_real_start:
    // qemu hands us the device tree pointer in x0. Keep it in x19 (nothing below
    // touches x19) until .bss is zeroed, then store it in __boot_dtb.
    mov     x19, x0

    // Only core 0 runs; park the others for now.
    mrs     x0, mpidr_el1
    and     x0, x0, #0xff
    cbnz    x0, 2f

    ldr     x0, =__stack_top
    mov     sp, x0

    // Zero .bss
    ldr     x0, =__bss_start
    ldr     x1, =__bss_end
1:  cmp     x0, x1
    b.hs    3f
    str     xzr, [x0], #8
    b       1b
3:  ldr     x0, =__boot_dtb
    str     x19, [x0]
    bl      kmain
2:  wfe
    b       2b

// Device tree pointer from the bootloader (0 if none). Lives in .data, not .bss,
// so it is a plain initialised word. Read it with mem::boot_dtb().
.section .data
.balign 8
.global __boot_dtb
__boot_dtb:
    .quad 0
