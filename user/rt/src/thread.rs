//! A small pthread-ish layer on clone + futex: `spawn`/`join`, `Mutex`, `Condvar`.
//!
//! A thread shares the process's address space and fds (CLONE_VM | CLONE_FS | CLONE_FILES
//! | CLONE_SIGHAND | CLONE_THREAD). Its stack is an anonymous mmap; TLS (TPIDR_EL0) points
//! at a tiny control block whose first word is the thread's futex-join word, which the
//! kernel clears and wakes on exit (CLONE_CHILD_CLEARTID). No TLS variables, no
//! cancellation, no detaching: every thread must be joined.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use crate::sys;

const STACK_SIZE: usize = 128 * 1024;

const CLONE_VM: usize = 0x100;
const CLONE_FS: usize = 0x200;
const CLONE_FILES: usize = 0x400;
const CLONE_SIGHAND: usize = 0x800;
const CLONE_THREAD: usize = 0x10000;
const CLONE_SETTLS: usize = 0x80000;
const CLONE_CHILD_CLEARTID: usize = 0x200000;

/// Per-thread control block. `tid` is the kernel clears-to-0 join word (and the TLS base).
#[repr(C)]
struct Tcb {
    tid: AtomicI32,
    stack: *mut u8,
    stack_size: usize,
    /// Boxed closure to run.
    func: *mut (dyn FnMut() + 'static),
}

pub struct JoinHandle {
    tcb: *mut Tcb,
}

// SAFETY: the closure ran on its own thread; join synchronises before we touch the Tcb.
unsafe impl Send for JoinHandle {}

/// The entry trampoline every thread starts at: TPIDR_EL0 holds the Tcb pointer.
#[unsafe(naked)]
extern "C" fn thread_entry() -> ! {
    core::arch::naked_asm!("mrs x0, tpidr_el0", "bl {run}", "brk #1", run = sym thread_run);
}

extern "C" fn thread_run(tcb: *mut Tcb) -> ! {
    // SAFETY: `tcb` is this thread's control block, alive until join.
    let func = unsafe { (*tcb).func };
    // SAFETY: the boxed closure, called once.
    unsafe { (*func)() };
    sys::exit(0) // clears the join word and futex-wakes the joiner (CLONE_CHILD_CLEARTID)
}

/// Spawn a thread running `f`. Panics if out of memory.
pub fn spawn<F: FnMut() + Send + 'static>(f: F) -> JoinHandle {
    spawn_sized(STACK_SIZE, f)
}

/// [`spawn`] with a `stack_size`-byte stack (at least 64 KiB; deep work like a TLS
/// handshake wants more than the default 128 KiB).
pub fn spawn_sized<F: FnMut() + Send + 'static>(stack_size: usize, f: F) -> JoinHandle {
    let stack_size = stack_size.max(64 * 1024).next_multiple_of(4096);
    let stack = sys::mmap_anon(stack_size).expect("thread stack");
    // Put the Tcb at the base of the stack region (lowest address), the stack grows down
    // from the top.
    let tcb = stack as *mut Tcb;
    let boxed: alloc::boxed::Box<dyn FnMut() + 'static> = alloc::boxed::Box::new(f);
    // SAFETY: fresh, writable mapping.
    unsafe {
        (*tcb).tid = AtomicI32::new(-1);
        (*tcb).stack = stack;
        (*tcb).stack_size = stack_size;
        (*tcb).func = alloc::boxed::Box::into_raw(boxed);
    }
    let sp = (stack as usize + stack_size) & !15;
    let flags = CLONE_VM
        | CLONE_FS
        | CLONE_FILES
        | CLONE_SIGHAND
        | CLONE_THREAD
        | CLONE_SETTLS
        | CLONE_CHILD_CLEARTID;
    let ctid = unsafe { &(*tcb).tid } as *const AtomicI32 as usize;
    // clone(flags, stack, ptid, ctid, tls): the kernel sets this thread's TPIDR_EL0 = tls,
    // starts it at `thread_entry` on `sp`.
    let tid = clone(
        flags,
        sp,
        0,
        ctid,
        tcb as usize,
        thread_entry as *const () as usize,
    );
    if tid < 0 {
        panic!("clone: {}", sys::Errno(-tid as i32));
    }
    unsafe { (*tcb).tid.store(tid as i32, Ordering::Release) };
    JoinHandle { tcb }
}

impl JoinHandle {
    pub fn join(self) {
        let tcb = self.tcb;
        // SAFETY: tcb alive; the kernel clears `tid` to 0 and futex-wakes it on exit.
        let word = unsafe { &(*tcb).tid };
        loop {
            let v = word.load(Ordering::Acquire);
            if v == 0 {
                break;
            }
            let _ = sys::futex_wait(word.as_ptr() as usize, v as u32, None);
        }
        // SAFETY: the thread has exited; reclaim its closure and stack.
        unsafe {
            drop(alloc::boxed::Box::from_raw((*tcb).func));
            let stack = (*tcb).stack;
            let _ = sys::munmap(stack, (*tcb).stack_size);
        }
    }
}

/// Raw clone for a new thread. `entry` is where it starts; `sp` its stack; `tls` its
/// TPIDR_EL0. We can't let the compiler return into the child (it has a fresh stack), so
/// this is a small asm wrapper: in the child we jump straight to `entry`.
fn clone(flags: usize, sp: usize, ptid: usize, ctid: usize, tls: usize, entry: usize) -> isize {
    let ret: isize;
    // SAFETY: syscall; in the child (x0==0) we branch to `entry` and never come back here.
    unsafe {
        core::arch::asm!(
            "mov x8, #220",
            "svc #0",
            "cbnz x0, 2f",   // parent: x0 = child tid or -errno
            "br {entry}",    // child: x0 was 0, jump to the entry trampoline
            "2:",
            entry = in(reg) entry,
            inout("x0") flags => ret,
            in("x1") sp,
            in("x2") ptid,
            in("x3") ctid,
            in("x4") tls,
            out("x8") _,
            clobber_abi("C"),
        );
    }
    ret
}

// ---- Mutex and Condvar on futexes --------------------------------------------------

/// A futex-backed mutex. 0 = free, 1 = held, 2 = held and contended.
pub struct Mutex<T> {
    state: AtomicU32,
    data: UnsafeCell<T>,
}

// SAFETY: access to `data` is serialised by `state`.
unsafe impl<T: Send> Sync for Mutex<T> {}
unsafe impl<T: Send> Send for Mutex<T> {}

pub struct MutexGuard<'a, T> {
    m: &'a Mutex<T>,
}

impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            state: AtomicU32::new(0),
            data: UnsafeCell::new(data),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        // Fast path: 0 -> 1.
        if self
            .state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.lock_slow();
        }
        MutexGuard { m: self }
    }

    fn lock_slow(&self) {
        let addr = self.state.as_ptr() as usize;
        loop {
            // Mark contended and sleep while held.
            if self.state.swap(2, Ordering::Acquire) == 0 {
                return; // we got it
            }
            let _ = sys::futex_wait(addr, 2, None);
        }
    }

    fn unlock(&self) {
        if self.state.swap(0, Ordering::Release) == 2 {
            // Someone may be waiting.
            let _ = sys::futex_wake(self.state.as_ptr() as usize, 1);
        }
    }
}

impl<T> core::ops::Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: we hold the lock.
        unsafe { &*self.m.data.get() }
    }
}
impl<T> core::ops::DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: we hold the lock.
        unsafe { &mut *self.m.data.get() }
    }
}
impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.m.unlock();
    }
}

/// A condition variable: a futex sequence bumped on every notify.
pub struct Condvar {
    seq: AtomicU32,
}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}

impl Condvar {
    pub const fn new() -> Self {
        Self {
            seq: AtomicU32::new(0),
        }
    }

    /// Atomically unlock `guard` and wait until notified, then re-lock.
    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        let m = guard.m;
        let s = self.seq.load(Ordering::Acquire);
        drop(guard); // release the mutex
        let _ = sys::futex_wait(self.seq.as_ptr() as usize, s, None);
        m.lock()
    }

    pub fn notify_one(&self) {
        self.seq.fetch_add(1, Ordering::Release);
        let _ = sys::futex_wake(self.seq.as_ptr() as usize, 1);
    }

    pub fn notify_all(&self) {
        self.seq.fetch_add(1, Ordering::Release);
        let _ = sys::futex_wake(self.seq.as_ptr() as usize, i32::MAX as usize);
    }
}
