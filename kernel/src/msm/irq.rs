//! Interrupts and the timer for the Pixel 2, the step of stage 2 that can fail on a phone in ways only the screen
//! tells: the GICv3 comes up (gic3.rs), then the scheduler tick's timer is *tried* before it is relied on.
//!
//! The tree names the four timer interrupts of the generic timer (`arm,armv8-timer`: secure physical, non-secure
//! physical, virtual, hypervisor). The kernel is an EL1 kernel under Qualcomm's hypervisor, so the virtual timer is
//! the normal choice. Instead of trusting that, [`init`] arms the comparator 1 ms ahead with the CPU's interrupts masked
//! and watches the GIC: the timer's own condition (`ISTATUS`) and which private interrupt became pending. A timer
//! that fires but whose interrupt the GIC does not show is skipped; one whose interrupt shows up under another
//! number than the tree said is used under that number. Everything seen goes in the log, so one photograph of the
//! screen says which timer and which INTID the phone uses.

use crate::dtb::Dtb;
use crate::kprintln;
use rpikit::msm::{TimerIntids, timer_intids};

/// The generic timer's interrupts from the tree, or the ARM-recommended ones when it has none.
pub fn tree_timer(dtb: Option<&Dtb>) -> (TimerIntids, bool) {
    let found = dtb
        .and_then(|d| d.prop_compat(b"arm,armv8-timer", b"interrupts"))
        .map(timer_intids);
    match found {
        Some(t) if t.virt.is_some() || t.phys.is_some() => (t, true),
        _ => (
            TimerIntids { secure: Some(29), phys: Some(30), virt: Some(27), hyp: Some(26) },
            false,
        ),
    }
}

/// GIC, timer, tick. `Err` is a line for the screen: the kernel then stays in stage 1.
pub fn init() -> Result<(), &'static str> {
    let opts = super::opts();
    // SAFETY: the bootloader's pointer, or 0.
    let dtb = unsafe { Dtb::from_addr(crate::mem::boot_dtb_ptr()) };
    let (tree, from_tree) = tree_timer(dtb.as_ref());
    let show = |v: Option<u32>| v.map_or(0, |x| x);
    kprintln!(
        "irq: timer interrupts {}: secure {} physical {} virtual {} hypervisor {}",
        if from_tree { "from the tree" } else { "ASSUMED, the tree has no armv8-timer node" },
        show(tree.secure),
        show(tree.phys),
        show(tree.virt),
        show(tree.hyp)
    );
    crate::gic::init_v3()?;
    let mut seen = [None, None];
    let chosen = crate::timer::select_msm(opts.timer, tree, &mut seen);
    for p in seen.iter().flatten() {
        kprintln!(
            "irq: {} timer, INTID {}: condition {}, private interrupts pending in the GIC {:#010x}",
            p.name,
            p.intid,
            if p.fired { "met" } else { "NOT met" },
            p.pending
        );
    }
    let Some((name, intid)) = chosen else {
        return Err("no timer interrupt reaches this CPU (the GIC shows nothing for the timers)");
    };
    kprintln!("irq: scheduler tick on the {name} timer, INTID {intid}");
    crate::timer::init();
    Ok(())
}
