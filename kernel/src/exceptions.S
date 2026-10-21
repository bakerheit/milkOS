// milkOS exception vectors. Included by exceptions.rs via global_asm! (braces are
// format placeholders filled with Rust consts/symbols).
//
// Every vector slot saves a full TrapFrame on the kernel stack (SP_EL1), then calls
// exception_entry(frame: *mut TrapFrame, vector: usize). x1 carries the vector number
// (0..15) and must stay untouched until the call.

.equ FRAME_SIZE, {FRAME_SIZE}
.equ OFF_X30,    {OFF_X30}
.equ OFF_SP_EL0, {OFF_SP_EL0}
.equ OFF_ELR,    {OFF_ELR}
.equ OFF_SPSR,   {OFF_SPSR}
.equ OFF_ESR,    {OFF_ESR}
.equ OFF_FAR,    {OFF_FAR}

.macro VECTOR idx
    .balign 0x80
    sub     sp, sp, #FRAME_SIZE
    stp     x0, x1, [sp, #0]
    mov     x1, #\idx
    b       exc_common
.endm

.section .text.vectors, "ax"
.balign 0x800
.global exception_vectors
exception_vectors:
    // Current EL with SP_EL0
    VECTOR 0    // sync
    VECTOR 1    // irq
    VECTOR 2    // fiq
    VECTOR 3    // serror
    // Current EL with SP_ELx (the kernel's normal state)
    VECTOR 4
    VECTOR 5
    VECTOR 6
    VECTOR 7
    // Lower EL, AArch64
    VECTOR 8
    VECTOR 9
    VECTOR 10
    VECTOR 11
    // Lower EL, AArch32
    VECTOR 12
    VECTOR 13
    VECTOR 14
    VECTOR 15

.section .text, "ax"
exc_common:
    stp     x2,  x3,  [sp, #16]
    stp     x4,  x5,  [sp, #32]
    stp     x6,  x7,  [sp, #48]
    stp     x8,  x9,  [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    str     x30,      [sp, #OFF_X30]
    mrs     x2, sp_el0
    mrs     x3, elr_el1
    mrs     x4, spsr_el1
    mrs     x5, esr_el1
    mrs     x6, far_el1
    str     x2,       [sp, #OFF_SP_EL0]
    stp     x3, x4,   [sp, #OFF_ELR]
    stp     x5, x6,   [sp, #OFF_ESR]
    mov     x0, sp
    bl      {HANDLER}

// Restore the TrapFrame at sp and eret. Also the way back for a scheduler: point sp at
// any frame (see restore_frame below) and branch here.
.global exc_return
exc_return:
    ldr     x2,       [sp, #OFF_SP_EL0]
    ldp     x3, x4,   [sp, #OFF_ELR]
    msr     sp_el0, x2
    msr     elr_el1, x3
    msr     spsr_el1, x4
    ldp     x0,  x1,  [sp, #0]
    ldp     x2,  x3,  [sp, #16]
    ldp     x4,  x5,  [sp, #32]
    ldp     x6,  x7,  [sp, #48]
    ldp     x8,  x9,  [sp, #64]
    ldp     x10, x11, [sp, #80]
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x19, [sp, #144]
    ldp     x20, x21, [sp, #160]
    ldp     x22, x23, [sp, #176]
    ldp     x24, x25, [sp, #192]
    ldp     x26, x27, [sp, #208]
    ldp     x28, x29, [sp, #224]
    ldr     x30,      [sp, #OFF_X30]
    add     sp, sp, #FRAME_SIZE
    eret

// extern "C" fn restore_frame(frame: *const TrapFrame) -> !
// Switch the kernel stack to `frame` and resume whatever it describes.
.global restore_frame
restore_frame:
    mov     sp, x0
    b       exc_return
