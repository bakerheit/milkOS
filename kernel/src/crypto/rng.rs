//! The kernel CSPRNG: ChaCha20 with fast key erasure, behind `/dev/random`, `/dev/urandom`
//! and getrandom(278). See docs/crypto.md.
//!
//! * **Generator.** A 256-bit ChaCha20 key. Every request (up to 4 KiB at a time) runs
//!   ChaCha20 under that key (nonce 0, counter from 0): the first 32 bytes of the keystream
//!   become the next key at once, the rest is the output, and the old key and the scratch
//!   blocks are wiped. A key is never used twice and can't be recovered from a later state,
//!   so a memory dump doesn't reveal earlier output (forward secrecy).
//! * **Entropy pool.** A SHA-256 state that every input is hashed into: virtio-rng bytes
//!   (the host's /dev/urandom), timer jitter samples, writes to /dev/random, the counter at
//!   each reseed. **Reseed**: key = SHA-256("milkOS rng reseed" || key || pool digest ||
//!   counter). That happens at boot, then on the next request after 60 s or 1 MiB of
//!   output, with 32 fresh virtio-rng bytes and some jitter.
//! * **Seeding.** [`init`] (boot, right after the virtio probe) takes 64 bytes from
//!   virtio-rng plus 4096 jitter samples. Without virtio-rng it seeds from jitter alone and
//!   says so, loudly: in a VM the counter's jitter is a weak source.
//!   Readers block only until that first seed ([`fill`]); nothing blocks after that.
//! * **Concurrency.** One generator behind a spinlock (no per-CPU state). Requests are cut
//!   into 4 KiB pieces, so the lock is held for about 64 ChaCha20 blocks at a time.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use milkcrypto::chacha20;
use milkcrypto::sha256::Sha256;

use crate::sched::WaitQueue;
use crate::sync::SpinLock;
use crate::virtio::rng as vrng;
use crate::{kprintln, timer};

const RESEED_NS: u64 = 60_000_000_000;
const RESEED_BYTES: u64 = 1 << 20;
const PIECE: usize = 4096;

struct State {
    key: [u8; 32],
    pool: Sha256,
    /// Bytes handed out since the last reseed.
    out_since: u64,
    last_reseed: u64,
}

static RNG: SpinLock<State> = SpinLock::new(State {
    key: [0; 32],
    pool: Sha256::new(),
    out_since: 0,
    last_reseed: 0,
});
static SEEDED: AtomicBool = AtomicBool::new(false);
static SEED_WQ: WaitQueue = WaitQueue::new();
static RESEEDS: AtomicU64 = AtomicU64::new(0);
static OUTPUT: AtomicU64 = AtomicU64::new(0);
static FROM_DEVICE: AtomicU64 = AtomicU64::new(0);
/// Seeded without a hardware source (jitter only).
static WEAK: AtomicBool = AtomicBool::new(false);

/// Hash `samples` timing samples into `h`: the counter around a small data-dependent
/// workload. In a VM the counter ticks at 24-62.5 MHz and the jitter is low: counted as
/// a supplement, never as the only source unless there is nothing else.
fn jitter(h: &mut Sha256, samples: usize) {
    let mut scratch = [0u64; 64];
    let mut x = timer::counter();
    for i in 0..samples {
        let t0 = timer::counter();
        for k in 0..(8 + (x as usize & 31)) {
            let j = (x as usize ^ k) & 63;
            scratch[j] = scratch[j].wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ x;
            x = x.rotate_left(7) ^ scratch[(j + 1) & 63];
        }
        let t1 = timer::counter();
        h.update(&(t1.wrapping_sub(t0) ^ (t1 << 20) ^ i as u64).to_le_bytes());
    }
    h.update(&x.to_le_bytes());
}

/// Fresh device bytes into the pool (outside the RNG lock: the device read spins).
fn device_entropy(n: usize) -> ([u8; 64], usize) {
    let mut b = [0u8; 64];
    let mut got = 0;
    while got < n {
        let k = vrng::read(&mut b[got..n], 100_000_000);
        // The Pi has the SoC's generator instead of virtio-rng.
        #[cfg(feature = "raspi")]
        let k = if k == 0 {
            crate::rpi::rng::read(&mut b[got..n], 100_000_000)
        } else {
            k
        };
        if k == 0 {
            break;
        }
        got += k;
    }
    FROM_DEVICE.fetch_add(got as u64, Ordering::Relaxed);
    (b, got)
}

fn reseed(st: &mut State) {
    st.pool.update(&timer::counter().to_le_bytes());
    let digest = core::mem::take(&mut st.pool).finish();
    let mut h = Sha256::new();
    h.update(b"milkOS rng reseed");
    h.update(&st.key);
    h.update(&digest);
    h.update(&RESEEDS.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    st.key = h.finish();
    // The pool keeps chaining: the old digest seeds the next one.
    st.pool = Sha256::new();
    st.pool.update(&digest);
    st.out_since = 0;
    st.last_reseed = timer::now_ns();
}

/// Mix bytes into the pool (no entropy credited): /dev/random writes, etc.
pub fn add_entropy(data: &[u8]) {
    RNG.lock().pool.update(data);
}

/// Bytes from a hardware source driven by a user-space driver (rngd through /dev/entropy,
/// docs/drivers.md): mixed in like the kernel driver's and counted as device bytes. They
/// never replace the pool, so a broken driver can't make it worse than jitter alone.
pub fn add_device_entropy(data: &[u8]) {
    RNG.lock().pool.update(data);
    FROM_DEVICE.fetch_add(data.len() as u64, Ordering::Relaxed);
    FROM_USER_DRIVER.fetch_add(data.len() as u64, Ordering::Relaxed);
}

static FROM_USER_DRIVER: AtomicU64 = AtomicU64::new(0);

/// Seed the generator. Boot, after `virtio::init` (so virtio-rng is attached).
pub fn init() {
    let mut pool = Sha256::new();
    let (dev, got) = device_entropy(64);
    pool.update(&dev[..got]);
    jitter(&mut pool, 4096);
    pool.update(&timer::counter().to_le_bytes());
    {
        let mut st = RNG.lock();
        st.pool = pool;
        reseed(&mut st);
    }
    let mut d = dev;
    milkcrypto::wipe(&mut d);
    if got == 0 {
        WEAK.store(true, Ordering::Release);
        kprintln!("[rng] ************************************************************");
        kprintln!(
            "[rng] WARNING: no virtio-rng device or hardware generator. The CSPRNG is seeded from timer"
        );
        kprintln!("[rng] jitter ONLY, which is weak: keys and nonces made on this boot may be");
        kprintln!(
            "[rng] guessable. (qemu: boot with -device virtio-rng-device; qemu's raspi4b has none.)"
        );
        kprintln!("[rng] ************************************************************");
    }
    SEEDED.store(true, Ordering::Release);
    SEED_WQ.wake_all();
    selftest(got);
}

pub fn seeded() -> bool {
    SEEDED.load(Ordering::Acquire)
}

/// Fill `out` with random bytes. Blocks (thread context) only until the first seed.
pub fn fill(out: &mut [u8]) {
    if !seeded() {
        SEED_WQ.wait_until(seeded);
    }
    let due = {
        let st = RNG.lock();
        st.out_since >= RESEED_BYTES || timer::now_ns().saturating_sub(st.last_reseed) >= RESEED_NS
    };
    if due {
        let (mut b, got) = device_entropy(32);
        let mut j = Sha256::new();
        jitter(&mut j, 64);
        let jd = j.finish();
        let mut st = RNG.lock();
        st.pool.update(&b[..got]);
        st.pool.update(&jd);
        reseed(&mut st);
        drop(st);
        milkcrypto::wipe(&mut b);
    }
    for piece in out.chunks_mut(PIECE) {
        generate(piece);
    }
    OUTPUT.fetch_add(out.len() as u64, Ordering::Relaxed);
}

/// One piece (<= 4 KiB) with fast key erasure.
fn generate(out: &mut [u8]) {
    let mut st = RNG.lock();
    let nonce = [0u8; 12];
    let mut blk = chacha20::block(&st.key, 0, &nonce);
    let mut next = [0u8; 32];
    next.copy_from_slice(&blk[..32]);
    let first = out.len().min(32);
    out[..first].copy_from_slice(&blk[32..32 + first]);
    for (counter, chunk) in (1u32..).zip(out[first..].chunks_mut(64)) {
        blk = chacha20::block(&st.key, counter, &nonce);
        chunk.copy_from_slice(&blk[..chunk.len()]);
    }
    st.key = next;
    st.out_since += out.len() as u64;
    drop(st);
    milkcrypto::wipe(&mut blk);
    milkcrypto::wipe(&mut next);
}

/// A random u64 (kernel users: TCP sequence numbers, ports, ...).
pub fn next_u64() -> u64 {
    let mut b = [0u8; 8];
    fill(&mut b);
    u64::from_le_bytes(b)
}

/// `/proc`-style one-line status.
pub fn status() -> alloc::string::String {
    let source = if crate::udrv::user_owned(crate::udrv::Class::Rng) {
        "rngd+jitter"
    } else if vrng::present() {
        "virtio-rng+jitter"
    } else if FROM_DEVICE.load(Ordering::Relaxed) > 0 {
        "jitter (no driver right now)"
    } else {
        "jitter-only(WEAK)"
    };
    alloc::format!(
        "seeded={} source={source} reseeds={} device_bytes={} user_driver_bytes={} output_bytes={}\n",
        seeded() as u8,
        RESEEDS.load(Ordering::Relaxed),
        FROM_DEVICE.load(Ordering::Relaxed),
        FROM_USER_DRIVER.load(Ordering::Relaxed),
        OUTPUT.load(Ordering::Relaxed)
    )
}

/// Boot check: the ChaCha20 vectors (the shared crate's), forward secrecy (the key moves
/// on every request), and a statistical sanity check of 64 KiB of output: the share of
/// one bits, a chi-square over byte values, no repeated 16-byte blocks. Catches a broken
/// generator, not a weak seed.
fn selftest(device_bytes: usize) {
    let (cp, ct) = chacha20::selftest();
    let k0 = RNG.lock().key;
    let mut a = [0u8; 32];
    fill(&mut a);
    let k1 = RNG.lock().key;
    let mut b = [0u8; 32];
    fill(&mut b);
    let moved = k0 != k1 && a != b;
    let mut buf = alloc::vec![0u8; 64 * 1024];
    fill(&mut buf);
    let ones: u64 = buf.iter().map(|x| x.count_ones() as u64).sum();
    let bits = buf.len() as u64 * 8;
    let mut hist = [0u64; 256];
    for &x in &buf {
        hist[x as usize] += 1;
    }
    // chi^2 * 10 in integers: sum (n - e)^2 / e, e = len / 256.
    let e = buf.len() as u64 / 256;
    let chi10: u64 = hist.iter().map(|&n| (n.abs_diff(e)).pow(2) * 10 / e).sum();
    let mut blocks: alloc::vec::Vec<[u8; 16]> =
        buf.chunks(16).map(|c| c.try_into().unwrap()).collect();
    blocks.sort_unstable();
    let repeats = blocks.windows(2).filter(|w| w[0] == w[1]).count();
    // 524288 bits: sigma = 362 bits, allow 5 sigma; chi^2 with 255 dof: mean 255, sd 22.6,
    // allow < 400 (p ~ 1e-9).
    let ones_ok = ones.abs_diff(bits / 2) <= 5 * 362;
    let chi_ok = chi10 < 4000;
    let ok = cp == ct && moved && ones_ok && chi_ok && repeats == 0;
    kprintln!(
        "[rng] ChaCha20 CSPRNG seeded from {} + 4096 jitter samples; chacha20 {cp}/{ct} vectors, \
         key erasure {}, 64 KiB: {}.{:02}% ones, chi2 {}.{} (255 dof), {repeats} repeated blocks: {}",
        if device_bytes > 0 {
            alloc::format!("virtio-rng ({device_bytes} bytes)")
        } else {
            alloc::string::String::from("NO hardware source")
        },
        if moved { "ok" } else { "BROKEN" },
        ones * 100 / bits,
        ones * 10000 / bits % 100,
        chi10 / 10,
        chi10 % 10,
        if ok { "ok" } else { "FAILED" }
    );
    milkcrypto::wipe(&mut buf);
}
