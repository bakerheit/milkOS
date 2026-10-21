// Secondary CPU entry (PSCI CPU_ON). EL1, MMU and caches off, x0 = &BootBlock (physical,
// which is also its virtual address: the kernel is identity mapped).
// BootBlock: stack_top @0, mair @8, tcr @16, ttbr0 @24, sctlr @32, cpu @40.

.section .text, "ax"
.global secondary_entry
secondary_entry:
    mov     x19, x0
    msr     daifset, #0xf
    ldr     x1, [x19, #8]
    msr     mair_el1, x1
    ldr     x1, [x19, #16]
    msr     tcr_el1, x1
    ldr     x1, [x19, #24]
    msr     ttbr0_el1, x1
    msr     ttbr1_el1, xzr
    isb
    tlbi    vmalle1
    ic      iallu
    dsb     nsh
    isb
    ldr     x1, [x19, #32]
    dsb     sy
    msr     sctlr_el1, x1          // MMU + caches on, same tables as the boot CPU
    isb
    ldr     x1, [x19, #0]
    mov     sp, x1
    ldr     x0, [x19, #40]
    msr     tpidr_el1, x0          // per-CPU index (percpu.rs)
    mov     x29, xzr
    mov     x30, xzr
    bl      {MAIN}
1:  wfe
    b       1b
