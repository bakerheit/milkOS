// milkOS entry point. qemu -kernel jumps here at EL1 with the MMU off.
.section .text.boot
.global _start
_start:
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
3:  bl      kmain
2:  wfe
    b       2b
