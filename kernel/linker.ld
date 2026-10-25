/* milkOS kernel image. qemu's virt machine has RAM at 0x4000_0000 and loads the image
   at RAM + 0x80000 (text_offset in boot.S). The kernel runs in the higher half: it is
   linked at KERNEL_BASE + that load address (mmu.rs KERNEL_BASE), loaded at the
   physical one. boot.S runs position-independent until the MMU is on. */
ENTRY(_start)

KERNEL_PHYS = 0x40080000;
KERNEL_BASE = 0xFFFF000000000000;

/* For boot.S's relocation (Pixel 2 builds): the physical address the image is linked for, and how
   many bytes of it a loader brings (everything before .bss). */
__load_phys = KERNEL_PHYS;

SECTIONS
{
    . = KERNEL_BASE + KERNEL_PHYS;
    __kernel_start = .;

    /* W^X (mmu.rs): text RX, rodata R, data/bss/stack RW + XN, so each starts on a page. */
    .text : AT(KERNEL_PHYS) {
        __text_start = .;
        KEEP(*(.text.boot))
        *(.text .text.*)
    }
    . = ALIGN(4K);
    __text_end = .;
    __rodata_start = .;
    .rodata : { *(.rodata .rodata.*) }
    . = ALIGN(4K);
    __rodata_end = .;
    __data_start = .;
    .data   : ALIGN(16) { *(.data .data.*) }
    .bss    : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    /* Boot stack (later thread 0, "kmain"). Like every kernel stack it is the upper 64 KiB
       of a 128 KiB-aligned slot, so the overflow check in exceptions.S (bit 16 of sp) works
       on it too. See kernel/src/sched/kstack.rs. */
    . = ALIGN(128K);
    . += 64K;
    __stack_bottom = .;
    . += 64K;
    __stack_top = .;
    __kernel_end = .;
    __image_size = __kernel_end - __kernel_start;
    __copy_len = __bss_start - __kernel_start;

    /DISCARD/ : { *(.comment) *(.eh_frame*) }
}
