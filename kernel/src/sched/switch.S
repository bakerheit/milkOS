// Context switch and thread entry trampolines. Included by sched/mod.rs via global_asm!
// (braces are format placeholders for Rust symbols).
//
// Context layout (sched::Context, repr(C)): x19..x30 at 0..96, sp at 96.

.section .text, "ax"

// extern "C" fn switch_context(prev: *mut Context, next: *const Context)
// Save the callee-saved registers, lr and sp of the running thread into `prev`, load
// `next`'s and return into it. Everything else is caller-saved, so the compiler has already
// spilled it. Called with IRQs masked.
.global switch_context
switch_context:
    stp     x19, x20, [x0, #0]
    stp     x21, x22, [x0, #16]
    stp     x23, x24, [x0, #32]
    stp     x25, x26, [x0, #48]
    stp     x27, x28, [x0, #64]
    stp     x29, x30, [x0, #80]
    mov     x9, sp
    str     x9, [x0, #96]
    ldp     x19, x20, [x1, #0]
    ldp     x21, x22, [x1, #16]
    ldp     x23, x24, [x1, #32]
    ldp     x25, x26, [x1, #48]
    ldp     x27, x28, [x1, #64]
    ldp     x29, x30, [x1, #80]
    ldr     x9, [x1, #96]
    mov     sp, x9
    ret

// First switch into a new kernel thread "returns" here: x19 = entry, x20 = argument.
.global thread_trampoline
thread_trampoline:
    mov     x0, x19
    mov     x1, x20
    bl      {THREAD_START}
    brk     #1

// First switch into a new user thread: its first TrapFrame sits at sp (the top of its kernel
// stack). Finish the switch, then eret to EL0 through the normal exception return path.
.global user_trampoline
user_trampoline:
    bl      {FIRST_RUN}
    mov     x0, sp
    b       restore_frame

// FP/SIMD state of user threads (the kernel itself is soft-float and never touches it).
// fp_save(*mut FpState) / fp_restore(*const FpState): q0..q31 at 0..512, fpcr, fpsr at 512.
.arch_extension fp
.arch_extension simd
.global fp_save
fp_save:
    stp     q0, q1, [x0, #0]
    stp     q2, q3, [x0, #32]
    stp     q4, q5, [x0, #64]
    stp     q6, q7, [x0, #96]
    stp     q8, q9, [x0, #128]
    stp     q10, q11, [x0, #160]
    stp     q12, q13, [x0, #192]
    stp     q14, q15, [x0, #224]
    stp     q16, q17, [x0, #256]
    stp     q18, q19, [x0, #288]
    stp     q20, q21, [x0, #320]
    stp     q22, q23, [x0, #352]
    stp     q24, q25, [x0, #384]
    stp     q26, q27, [x0, #416]
    stp     q28, q29, [x0, #448]
    stp     q30, q31, [x0, #480]
    mrs     x1, fpcr
    mrs     x2, fpsr
    str     x1, [x0, #512]
    str     x2, [x0, #520]
    ret

.global fp_restore
fp_restore:
    ldp     q0, q1, [x0, #0]
    ldp     q2, q3, [x0, #32]
    ldp     q4, q5, [x0, #64]
    ldp     q6, q7, [x0, #96]
    ldp     q8, q9, [x0, #128]
    ldp     q10, q11, [x0, #160]
    ldp     q12, q13, [x0, #192]
    ldp     q14, q15, [x0, #224]
    ldp     q16, q17, [x0, #256]
    ldp     q18, q19, [x0, #288]
    ldp     q20, q21, [x0, #320]
    ldp     q22, q23, [x0, #352]
    ldp     q24, q25, [x0, #384]
    ldp     q26, q27, [x0, #416]
    ldp     q28, q29, [x0, #448]
    ldp     q30, q31, [x0, #480]
    ldr     x1, [x0, #512]
    ldr     x2, [x0, #520]
    msr     fpcr, x1
    msr     fpsr, x2
    ret
