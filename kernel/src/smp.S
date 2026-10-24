// Secondary CPU entry (PSCI CPU_ON, or released from the firmware's spin table). EL1 (EL2 on
// the Pi: the first thing it does is drop to EL1, el.S), MMU and caches off, running at the
// physical address, x0 = &BootBlock (physical).
// BootBlock: stack_top @0, mair @8, tcr @16, ttbr0 (boot identity tables) @24, sctlr @32,
// cpu @40, ttbr1 (kernel) @48, ttbr0_final (empty) @56.
// Turn the MMU on with the identity map in TTBR0 and the kernel in TTBR1, branch up to the
// kernel's virtual address, then drop the identity map.

.section .text, "ax"
// Entry for a CPU released from the firmware's spin table (EL2, MMU off, no argument): find
// the boot block by CPU number (MPIDR Aff0) in spin_blocks, then carry on as secondary_entry.
.global secondary_spin_entry
secondary_spin_entry:
    mrs     x0, mpidr_el1
    and     x0, x0, #0xff
    cmp     x0, #8
    b.hs    3f
    adrp    x1, spin_blocks
    add     x1, x1, :lo12:spin_blocks
    ldr     x0, [x1, x0, lsl #3]
    cbnz    x0, secondary_entry
3:  wfe
    b       3b

.global secondary_entry
secondary_entry:
    mov     x19, x0
    msr     daifset, #0xf
    drop_to_el1 x1
    ldr     x1, [x19, #8]
    msr     mair_el1, x1
    ldr     x1, [x19, #16]
    msr     tcr_el1, x1
    ldr     x1, [x19, #24]
    msr     ttbr0_el1, x1
    ldr     x1, [x19, #48]
    msr     ttbr1_el1, x1
    isb
    tlbi    vmalle1
    ic      iallu
    dsb     nsh
    isb
    ldr     x1, [x19, #32]
    dsb     sy
    msr     sctlr_el1, x1          // MMU + caches on
    isb
    // Read everything from the block while it is still identity mapped.
    ldr     x1, [x19, #0]
    ldr     x20, [x19, #40]
    ldr     x21, [x19, #56]
    ldr     x2, =1f
    br      x2
1:  mov     sp, x1
    msr     ttbr0_el1, x21         // identity map gone: TTBR0 is user space only
    isb
    tlbi    vmalle1
    dsb     nsh
    isb
    mrs     x2, sctlr_el1          // W^X in hardware (SCTLR.WXN), now that we are off the
    orr     x2, x2, #(1 << 19)     // writable+executable boot block
    msr     sctlr_el1, x2
    isb
    tlbi    vmalle1
    dsb     nsh
    isb
    mov     x0, x20
    msr     tpidr_el1, x0          // per-CPU index (percpu.rs)
    mov     x29, xzr
    mov     x30, xzr
    bl      {MAIN}
2:  wfe
    b       2b
.ltorg

// Boot block addresses for spin-table CPUs (smp.rs release_spin); in .data so it is plain
// initialised memory, 8 CPUs (percpu::MAX_CPUS).
.section .data
.balign 64
.global spin_blocks
spin_blocks:
    .space 64

.purgem drop_to_el1
