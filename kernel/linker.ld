/* milkOS kernel image. qemu's virt machine has RAM at 0x4000_0000. */
ENTRY(_start)

SECTIONS
{
    . = 0x40080000;
    __kernel_start = .;

    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }
    .rodata : ALIGN(16) { *(.rodata .rodata.*) }
    .data   : ALIGN(16) { *(.data .data.*) }
    .bss    : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    . = ALIGN(4096);
    __stack_bottom = .;
    . += 64K;
    __stack_top = .;
    __kernel_end = .;
    __image_size = __kernel_end - __kernel_start;

    /DISCARD/ : { *(.comment) *(.eh_frame*) }
}
