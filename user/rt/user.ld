/* milkOS user programs. The kernel puts user space at 1 TiB and up (mmu::USER_BASE);
   programs load at IMAGE_BASE = USER_BASE + 4 MiB (proc/uspace.rs). Text, read-only
   data and data each get their own page-aligned PT_LOAD so permissions are exact. */
ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5);   /* R X */
    rodata PT_LOAD FLAGS(4);   /* R   */
    data   PT_LOAD FLAGS(6);   /* R W */
}

SECTIONS
{
    . = 0x10000400000;
    .text : {
        KEEP(*(.text._start))
        *(.text .text.*)
    } :text

    . = ALIGN(4096);
    .rodata : { *(.rodata .rodata.*) } :rodata

    . = ALIGN(4096);
    .data : { *(.data .data.*) *(.got .got.*) } :data
    .bss : { *(.bss .bss.*) *(COMMON) } :data

    /DISCARD/ : { *(.comment) *(.eh_frame*) *(.note*) }
}
