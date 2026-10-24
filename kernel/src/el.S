// Exception level entry. The Raspberry Pi firmware (and qemu raspi4b) starts every core at
// EL2; qemu virt starts them at EL1. The kernel runs at EL1, so a core that finds itself at
// EL2 sets up the little the EL1 world needs from EL2 and drops with an eret. Already at EL1
// (virt): nothing happens, the macro is two instructions.
//
// Used by boot.S (the boot core) and smp.S (secondaries). Included in front of them by
// mmu.rs and smp.rs; the macro is removed again at the end so both can include it.
//
// What EL1 needs from EL2:
//  - HCR_EL2.RW = 1: EL1 is AArch64. Everything else in HCR_EL2 stays 0, which routes IRQs,
//    FIQs and aborts to EL1 and does not trap anything to EL2.
//  - CNTHCTL_EL2.EL1PCTEN and .EL1PCEN: EL1 may use the physical counter and timer; CNTVOFF_EL2 = 0:
//    the virtual counter is the physical one.
//  - VPIDR_EL2 / VMPIDR_EL2: EL1 reads the real MIDR / MPIDR (percpu::record_midr, the CPU number).
//  - CPTR_EL2, HSTR_EL2: no trapping of FP/SIMD or CP15-style registers.
//  - SCTLR_EL1: the architectural reset value, MMU and caches off.
//  - SPSR_EL2 = EL1h with D, A, I, F masked, ELR_EL2 = the next instruction.
.macro drop_to_el1 tmp
    mrs     \tmp, CurrentEL
    cmp     \tmp, #(2 << 2)
    b.ne    8f
    mov     \tmp, #(1 << 31)                // HCR_EL2.RW
    msr     hcr_el2, \tmp
    mrs     \tmp, cnthctl_el2
    orr     \tmp, \tmp, #3                  // EL1PCTEN | EL1PCEN
    msr     cnthctl_el2, \tmp
    msr     cntvoff_el2, xzr
    mrs     \tmp, midr_el1
    msr     vpidr_el2, \tmp
    mrs     \tmp, mpidr_el1
    msr     vmpidr_el2, \tmp
    mov     \tmp, #0x33ff                   // CPTR_EL2: RES1 bits only, nothing trapped
    msr     cptr_el2, \tmp
    msr     hstr_el2, xzr
    mov     \tmp, #0x0800                   // SCTLR_EL1 reset value (RES1 bits), MMU off
    movk    \tmp, #0x30d0, lsl #16
    msr     sctlr_el1, \tmp
    mov     \tmp, #0x3c5                    // EL1h, DAIF masked
    msr     spsr_el2, \tmp
    adr     \tmp, 8f
    msr     elr_el2, \tmp
    isb
    eret
8:
.endm
