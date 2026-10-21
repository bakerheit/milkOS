//! TCP (RFC 9293), with:
//!
//! * RFC 6298 retransmission timing (Karn, backoff), RFC 5681 slow start / congestion
//!   avoidance, RFC 5961 challenge ACKs, the MSS option;
//! * RFC 7323 window scaling (we offer shift [`RCV_WSCALE`]) and timestamps (RTT from
//!   every ACK's echo, PAWS against old duplicates);
//! * RFC 2018 SACK (receiver: up to 3-4 blocks, the newest first; sender: a scoreboard)
//!   with SACK-based loss recovery in the style of RFC 6675 (pipe vs cwnd, holes below the
//!   highest SACKed byte retransmitted first); without SACK, NewReno-style partial ACKs
//!   (RFC 6582); limited transmit (RFC 3042) and early retransmit for small flights
//!   (RFC 5827); sender-side silly window avoidance; cwnd in whole segments (SMSS);
//! * delayed ACKs (once more than a full segment is unacknowledged, else after
//!   [`DELACK`]; quick-ACK for a connection's first 16 segments; at once for
//!   out-of-order data, a filled hole, a FIN or a window update) and Nagle (RFC 896;
//!   off with TCP_NODELAY);
//! * auto-sized buffers: the receive buffer doubles when the application reads more than
//!   half of it per round trip, the send buffer when it is the limit; up to
//!   `socket::MAX_BUF`, unless SO_RCVBUF / SO_SNDBUF fixed them;
//! * TIME_WAIT for 2 x [`MSL`] (60 s) in a compact table ([`TwTable`]) that doesn't hold a
//!   socket slot or buffers; a new SYN for a TIME_WAIT pair with a higher sequence number
//!   (or timestamp) reuses it (RFC 6191), RSTs are ignored there (RFC 1337).
//!
//! Each connection's send buffer holds the bytes from `snd_una` on (unacknowledged and
//! unsent); its receive buffer holds what the user has not read (out-of-order data sits
//! past its readable end), and its free space is the window we advertise.

use super::Stack;
use super::Stats;
use super::errno::*;
use super::iface::{Iface, PROTO_TCP};
use super::ipv6::{self, State6};
use super::ring::Ring;
use super::socket::{
    MAX_BUF, POLLERR, POLLHUP, POLLIN, POLLOUT, POLLRDHUP, POLLRDNORM, POLLWRNORM, SockType,
};
use super::wire::{IpAddr, Ipv4, Ipv6, SockAddr, be16, be32, finish, l4_sum, put16, put32, sum};
use crate::fs::errno::{Errno, Result};

pub const MAX_BACKLOG: usize = 16;
/// MSS we announce and use at most (1500 MTU - 40).
pub const OUR_MSS: u16 = 1460;
/// The same over IPv6 (1500 - 40 - 20).
pub const OUR_MSS6: u16 = 1440;
/// MSS assumed when the peer sends no option (RFC 9293).
const DEFAULT_MSS: u16 = 536;
/// Our window scale shift: 65535 << 7 = 8 MiB, above `socket::MAX_BUF`.
pub const RCV_WSCALE: u8 = 7;

const MS: u64 = 1_000_000;
const RTO_INIT: u64 = 1000 * MS;
const RTO_MIN: u64 = 200 * MS;
const RTO_MAX: u64 = 60_000 * MS;
/// SYN / SYN-ACK retransmissions before giving up (1+2+4+8+16 s, then fail at ~31 s+32).
const SYN_RETRIES: u8 = 5;
/// Data retransmissions before the connection is aborted with ETIMEDOUT.
const DATA_RETRIES: u8 = 10;
/// Maximum segment lifetime: TIME_WAIT lasts 2 * MSL = 60 s (as Linux).
pub const MSL: u64 = 30_000 * MS;
/// An orphaned connection may sit in FIN_WAIT_2 this long (Linux tcp_fin_timeout).
const FIN_TIMEOUT: u64 = 30_000 * MS;
/// Delayed-ACK timer.
pub const DELACK: u64 = 40 * MS;
/// After a failed transmit (device or loopback queue full), try again this soon.
const KICK: u64 = 2 * MS;
/// PAWS: a timestamp older than this is not trusted any more (RFC 7323 5.5).
const PAWS_IDLE: u64 = 24 * 24 * 3600 * 1000 * MS;
/// Connections kept in TIME_WAIT at most (the oldest is dropped first when full).
pub const TW_MAX: usize = 512;

pub const FIN: u8 = 0x01;
pub const SYN: u8 = 0x02;
pub const RST: u8 = 0x04;
pub const PSH: u8 = 0x08;
pub const ACK: u8 = 0x10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
}

impl TcpState {
    /// Past the handshake (a peer exists and sequence numbers are agreed).
    pub fn synchronized(self) -> bool {
        !matches!(
            self,
            TcpState::Closed | TcpState::Listen | TcpState::SynSent | TcpState::SynReceived
        )
    }
    /// The user may still queue data.
    pub fn can_send(self) -> bool {
        matches!(self, TcpState::Established | TcpState::CloseWait)
    }
    /// The `st` column of `/proc/net/tcp`.
    pub fn linux_code(self) -> u8 {
        match self {
            TcpState::Established => 1,
            TcpState::SynSent => 2,
            TcpState::SynReceived => 3,
            TcpState::FinWait1 => 4,
            TcpState::FinWait2 => 5,
            TcpState::TimeWait => 6,
            TcpState::Closed => 7,
            TcpState::CloseWait => 8,
            TcpState::LastAck => 9,
            TcpState::Listen => 10,
            TcpState::Closing => 11,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            TcpState::Closed => "CLOSED",
            TcpState::Listen => "LISTEN",
            TcpState::SynSent => "SYN_SENT",
            TcpState::SynReceived => "SYN_RECV",
            TcpState::Established => "ESTABLISHED",
            TcpState::FinWait1 => "FIN_WAIT1",
            TcpState::FinWait2 => "FIN_WAIT2",
            TcpState::CloseWait => "CLOSE_WAIT",
            TcpState::Closing => "CLOSING",
            TcpState::LastAck => "LAST_ACK",
            TcpState::TimeWait => "TIME_WAIT",
        }
    }
}

const MAX_OOO: usize = 32;
const MAX_SACKED: usize = 16;

/// Transmission control block.
pub struct Tcb {
    pub state: TcpState,
    /// Has been connected (or accepted) once: a closed socket is not reusable.
    pub used: bool,
    pub iss: u32,
    pub snd_una: u32,
    pub snd_nxt: u32,
    /// Highest sequence number sent so far (snd_nxt rewinds on a timeout).
    pub snd_max: u32,
    /// Send window in bytes (already scaled).
    pub snd_wnd: u32,
    snd_wl1: u32,
    snd_wl2: u32,
    pub irs: u32,
    pub rcv_nxt: u32,
    /// Right edge of the window we last advertised.
    rcv_adv: u32,
    /// Send MSS (min of the peer's option and ours), not counting options.
    pub mss: u16,
    /// Window scaling in use: the peer's shift (applied to its windows) and ours.
    pub snd_wscale: u8,
    pub rcv_wscale: u8,
    /// Options both ends agreed on in the handshake.
    pub sack_ok: bool,
    pub ts_ok: bool,
    /// RFC 7323: the peer's latest timestamp worth echoing, when we took it, and the ACK
    /// number of the last ACK we sent.
    ts_recent: u32,
    ts_recent_at: u64,
    last_ack_sent: u32,
    /// The user is done sending: FIN goes out after the queued data.
    fin_queued: bool,
    /// Sequence number of our FIN, once sent.
    fin_seq: Option<u32>,
    fin_acked: bool,
    pub peer_fin: bool,
    /// Current retransmission timeout (backed off on timeouts).
    pub rto: u64,
    /// RTO from the RTT estimate, before backoff; restored on every ACK of new data.
    rto_base: u64,
    pub srtt: u64,
    rttvar: u64,
    /// Without timestamps: (sequence number that acks the timed segment, send time).
    /// Karn: never a resent one.
    rtt_probe: Option<(u32, u64)>,
    /// Retransmission (or persist) deadline.
    pub timer: Option<u64>,
    persist: bool,
    force_probe: bool,
    pub retries: u8,
    /// TIME_WAIT end, or the orphan FIN_WAIT_2 timeout.
    pub deadline: Option<u64>,
    /// Delayed ACK due.
    pub delack: Option<u64>,
    /// Retry sending (the device or loopback queue was full).
    pub kick: Option<u64>,
    /// Bytes received in order since our last ACK.
    unacked: u32,
    /// Quick-ACK mode (as Linux): the first segments of a connection are ACKed at once,
    /// so slow start isn't held up by delayed ACKs.
    quickack: u8,
    /// End of the last small (sub-MSS) segment sent: Nagle (Minshall's variant) holds a
    /// small segment only while an earlier small one is unacknowledged.
    snd_sml: u32,
    pub cwnd: u32,
    pub ssthresh: u32,
    dupacks: u8,
    ack_now: bool,
    /// SACK scoreboard (sender): ranges the peer has, absolute, in (snd_una, snd_max].
    sacked: [(u32, u32); MAX_SACKED],
    nsacked: u8,
    /// Loss recovery (RFC 6675 / 6582): until snd_una reaches `recover`; `high_rxt` is
    /// where retransmission has got to.
    pub in_recovery: bool,
    recover: u32,
    high_rxt: u32,
    /// The first retransmission of this recovery is still to go (it goes at once).
    rec_first: bool,
    /// Segments retransmitted by loss recovery (not by timeouts), all told.
    pub sack_retrans: u32,
    /// A connection not yet accepted: slot of its listener.
    pub parent: Option<u16>,
    accq: [u16; MAX_BACKLOG],
    accn: u8,
    pub backlog: u8,
    pub connect_pending: bool,
    pub retransmits: u32,
    /// Retransmission timeouts, and the longest RTO waited for (ms).
    pub timeouts: u32,
    pub max_rto_ms: u32,
    /// Out-of-order data already written into the receive ring past its end, as absolute
    /// sequence ranges [start, end); `ooo_last` is the newest (reported first in SACK).
    ooo: [(u32, u32); MAX_OOO],
    nooo: u8,
    ooo_last: u32,
    /// Receive buffer auto-sizing: bytes the user read since `rcv_mark`, and a
    /// receiver-side RTT estimate (from timestamp echoes).
    rcv_copied: usize,
    rcv_mark: u64,
    rcv_rtt: u64,
}

impl Tcb {
    pub const fn new() -> Tcb {
        Tcb {
            state: TcpState::Closed,
            used: false,
            iss: 0,
            snd_una: 0,
            snd_nxt: 0,
            snd_max: 0,
            snd_wnd: 0,
            snd_wl1: 0,
            snd_wl2: 0,
            irs: 0,
            rcv_nxt: 0,
            rcv_adv: 0,
            mss: DEFAULT_MSS,
            snd_wscale: 0,
            rcv_wscale: 0,
            sack_ok: false,
            ts_ok: false,
            ts_recent: 0,
            ts_recent_at: 0,
            last_ack_sent: 0,
            fin_queued: false,
            fin_seq: None,
            fin_acked: false,
            peer_fin: false,
            rto: RTO_INIT,
            rto_base: RTO_INIT,
            srtt: 0,
            rttvar: 0,
            rtt_probe: None,
            timer: None,
            persist: false,
            force_probe: false,
            retries: 0,
            deadline: None,
            delack: None,
            kick: None,
            unacked: 0,
            quickack: 16,
            snd_sml: 0,
            cwnd: 0,
            ssthresh: u32::MAX,
            dupacks: 0,
            ack_now: false,
            sacked: [(0, 0); MAX_SACKED],
            nsacked: 0,
            in_recovery: false,
            recover: 0,
            high_rxt: 0,
            rec_first: false,
            sack_retrans: 0,
            parent: None,
            accq: [0; MAX_BACKLOG],
            accn: 0,
            backlog: 0,
            connect_pending: false,
            retransmits: 0,
            timeouts: 0,
            max_rto_ms: 0,
            ooo: [(0, 0); MAX_OOO],
            nooo: 0,
            ooo_last: 0,
            rcv_copied: 0,
            rcv_mark: 0,
            rcv_rtt: 0,
        }
    }

    /// Children (in handshake or ready) queued on this listener.
    pub fn queued(&self) -> &[u16] {
        &self.accq[..self.accn as usize]
    }

    fn start(&mut self, iss: u32, mss: u16) {
        self.used = true;
        self.iss = iss;
        self.snd_sml = iss;
        self.snd_una = iss;
        self.snd_nxt = iss;
        self.snd_max = iss;
        self.mss = mss;
        self.cwnd = 10 * mss as u32; // RFC 6928 initial window
        self.ssthresh = u32::MAX;
        self.rto = RTO_INIT;
        self.rto_base = RTO_INIT;
    }

    fn sample_rtt(&mut self, r: u64) {
        if self.srtt == 0 {
            self.srtt = r.max(1);
            self.rttvar = r / 2;
        } else {
            let d = self.srtt.abs_diff(r);
            self.rttvar = (3 * self.rttvar + d) / 4;
            self.srtt = (7 * self.srtt + r) / 8;
        }
        self.rto_base = (self.srtt + (4 * self.rttvar).max(10 * MS)).clamp(RTO_MIN, RTO_MAX);
        self.rto = self.rto_base;
    }

    /// Option bytes on a data segment (timestamps, and SACK blocks while we hold
    /// out-of-order data), so data + options stay within the MSS.
    fn opt_len(&self) -> usize {
        let mut n = if self.ts_ok { 12 } else { 0 };
        if self.sack_ok && self.nooo > 0 {
            n += 4 + 8 * self.nooo.min(self.max_sack_blocks() as u8) as usize;
        }
        n
    }

    /// Sender MSS for congestion control: payload of a full segment with the options every
    /// segment carries (timestamps), so cwnd holds whole segments (RFC 5681 SMSS).
    fn smss(&self) -> u32 {
        (self.mss as u32)
            .saturating_sub(if self.ts_ok { 12 } else { 0 })
            .max(64)
    }

    fn max_sack_blocks(&self) -> usize {
        if self.ts_ok { 3 } else { 4 }
    }

    /// Payload bytes per full segment.
    fn max_data(&self) -> usize {
        (self.mss as usize).saturating_sub(self.opt_len()).max(64)
    }

    fn sacked_bytes(&self) -> u32 {
        self.sacked[..self.nsacked as usize]
            .iter()
            .map(|&(a, b)| b.wrapping_sub(a))
            .sum()
    }

    fn high_sack(&self) -> u32 {
        self.sacked[..self.nsacked as usize]
            .iter()
            .fold(self.snd_una, |m, &(_, b)| if gt(b, m) { b } else { m })
    }

    fn is_sacked(&self, x: u32) -> Option<u32> {
        self.sacked[..self.nsacked as usize]
            .iter()
            .find(|&&(a, b)| le(a, x) && lt(x, b))
            .map(|&(_, b)| b)
    }

    /// The next byte range to retransmit in recovery: an un-SACKed range at or after
    /// `high_rxt`, below the highest SACKed byte. Without SACK information, the segment at
    /// `snd_una` (NewReno), once per partial ACK.
    fn next_hole(&self) -> Option<(u32, u32)> {
        let mut x = if lt(self.high_rxt, self.snd_una) {
            self.snd_una
        } else {
            self.high_rxt
        };
        let high = self.high_sack();
        if self.nsacked == 0 {
            if x == self.snd_una && lt(x, self.snd_max) {
                return Some((x, self.snd_max));
            }
            return None;
        }
        while lt(x, high) {
            match self.is_sacked(x) {
                Some(end) => x = end,
                None => {
                    // Up to the next SACKed range.
                    let mut end = high;
                    for &(a, _) in &self.sacked[..self.nsacked as usize] {
                        if gt(a, x) && lt(a, end) {
                            end = a;
                        }
                    }
                    return Some((x, end));
                }
            }
        }
        None
    }

    /// RFC 6675 pipe: bytes we think are still in the network.
    fn pipe(&self) -> u32 {
        let flight = self.snd_max.wrapping_sub(self.snd_una);
        let mut lost = 0u32;
        if self.in_recovery && self.nsacked > 0 {
            // Holes not yet retransmitted, below the highest SACK, count as lost.
            let mut x = if lt(self.high_rxt, self.snd_una) {
                self.snd_una
            } else {
                self.high_rxt
            };
            let high = self.high_sack();
            while lt(x, high) {
                match self.is_sacked(x) {
                    Some(end) => x = end,
                    None => {
                        let mut end = high;
                        for &(a, _) in &self.sacked[..self.nsacked as usize] {
                            if gt(a, x) && lt(a, end) {
                                end = a;
                            }
                        }
                        lost = lost.wrapping_add(end.wrapping_sub(x));
                        x = end;
                    }
                }
            }
        }
        flight
            .saturating_sub(self.sacked_bytes())
            .saturating_sub(lost)
    }

    fn sack_insert(&mut self, mut a: u32, mut b: u32) {
        let mut k = 0;
        while k < self.nsacked as usize {
            let (s, e) = self.sacked[k];
            if le(s, b) && le(a, e) {
                if lt(s, a) {
                    a = s;
                }
                if gt(e, b) {
                    b = e;
                }
                self.sacked[k] = self.sacked[self.nsacked as usize - 1];
                self.nsacked -= 1;
            } else {
                k += 1;
            }
        }
        if (self.nsacked as usize) < MAX_SACKED {
            self.sacked[self.nsacked as usize] = (a, b);
            self.nsacked += 1;
        }
    }

    fn sack_prune(&mut self) {
        let una = self.snd_una;
        let mut k = 0;
        while k < self.nsacked as usize {
            let (a, b) = self.sacked[k];
            if le(b, una) {
                self.sacked[k] = self.sacked[self.nsacked as usize - 1];
                self.nsacked -= 1;
            } else {
                if lt(a, una) {
                    self.sacked[k].0 = una;
                }
                k += 1;
            }
        }
    }
}

// ---- sequence arithmetic --------------------------------------------------------------------

#[inline]
fn lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}
#[inline]
fn le(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) <= 0
}
#[inline]
fn gt(a: u32, b: u32) -> bool {
    lt(b, a)
}
#[inline]
fn ge(a: u32, b: u32) -> bool {
    le(b, a)
}
/// lo <= x < lo + len
#[inline]
fn in_window(x: u32, lo: u32, len: u32) -> bool {
    x.wrapping_sub(lo) < len
}

/// The timestamp clock: milliseconds.
fn tsval(now: u64) -> u32 {
    (now / MS) as u32
}

/// The secret in [`initial_seq`]: 64 random bits, drawn once (from the CSPRNG).
static ISN_SECRET: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn initial_seq(local: SockAddr, remote: SockAddr, now: u64) -> u32 {
    // RFC 6528: ISN = M + F(4-tuple, secret), M a 4 µs clock, F = SHA-256 keyed by a
    // boot-time secret, so sequence numbers can't be predicted from outside.
    use core::sync::atomic::Ordering;
    let mut key = ISN_SECRET.load(Ordering::Relaxed);
    if key == 0 {
        key = crate::crypto::rng::next_u64() | 1;
        ISN_SECRET.store(key, Ordering::Relaxed);
    }
    let mut h = crate::crypto::sha256::Sha256::new();
    h.update(&key.to_le_bytes());
    h.update(local.ip.bytes());
    h.update(&local.port.to_be_bytes());
    h.update(remote.ip.bytes());
    h.update(&remote.port.to_be_bytes());
    let d = h.finish();
    u32::from_le_bytes([d[0], d[1], d[2], d[3]]).wrapping_add((now / 4000) as u32)
}

// ---- segment output -------------------------------------------------------------------------

struct Hdr {
    seq: u32,
    ack: u32,
    flags: u8,
    wnd: u16,
}

/// TCP options of one segment.
#[derive(Default)]
struct Opts {
    /// SYN options: MSS, window scale (shift), SACK permitted.
    syn: bool,
    wscale: Option<u8>,
    sack_ok: bool,
    /// (TSval, TSecr).
    ts: Option<(u32, u32)>,
    sack: [(u32, u32); 4],
    nsack: usize,
}

impl Opts {
    fn write(&self, b: &mut [u8], mss: u16) -> usize {
        let mut k = 0;
        if self.syn {
            b[0] = 2;
            b[1] = 4;
            put16(b, 2, mss);
            k = 4;
            if self.sack_ok {
                b[k..k + 4].copy_from_slice(&[1, 1, 4, 2]);
                k += 4;
            }
        }
        if let Some((v, e)) = self.ts {
            b[k..k + 4].copy_from_slice(&[1, 1, 8, 10]);
            put32(b, k + 4, v);
            put32(b, k + 8, e);
            k += 12;
        }
        if let Some(s) = self.wscale {
            b[k..k + 4].copy_from_slice(&[1, 3, 3, s]);
            k += 4;
        }
        if self.nsack > 0 {
            b[k..k + 4].copy_from_slice(&[1, 1, 5, (2 + 8 * self.nsack) as u8]);
            k += 4;
            for &(l, r) in &self.sack[..self.nsack] {
                put32(b, k, l);
                put32(b, k + 4, r);
                k += 8;
            }
        }
        k
    }
}

/// Build and send one segment. `data`: (ring, offset from its head, length). False if it
/// could not be queued (device or loopback queue full).
/// The parts of the stack that sending a segment touches (so `output` can hold the
/// socket table at the same time).
struct Out<'a> {
    iface: &'a mut Iface,
    ip6: &'a mut State6,
    stats: &'a mut Stats,
    now: u64,
}

fn emit(
    out: &mut Out,
    local: SockAddr,
    remote: SockAddr,
    scope: Option<usize>,
    h: Hdr,
    o: &Opts,
    data: Option<(&Ring, usize, usize)>,
) -> bool {
    out.stats.tcp_out_segs += 1;
    out.stats.ip_out_requests += 1;
    if h.flags & RST != 0 {
        out.stats.tcp_out_rsts += 1;
    }
    let seg = |b: &mut [u8], src: IpAddr, dst: IpAddr| {
        put16(b, 0, local.port);
        put16(b, 2, remote.port);
        put32(b, 4, h.seq);
        put32(b, 8, h.ack);
        let ol = o.write(&mut b[20..60], our_mss(local.ip));
        let hl = 20 + ol;
        b[12] = ((hl / 4) << 4) as u8;
        b[13] = h.flags;
        put16(b, 14, h.wnd);
        put16(b, 16, 0);
        put16(b, 18, 0);
        let mut n = hl;
        if let Some((ring, off, len)) = data {
            let len = len.min(b.len() - hl);
            n += ring.peek_at(off, &mut b[hl..hl + len]);
        }
        let c = finish(sum(&b[..n], l4_sum(src, dst, PROTO_TCP, n)));
        put16(b, 16, c);
        n
    };
    match (local.ip, remote.ip) {
        (IpAddr::V6(l6), IpAddr::V6(r6)) => ipv6::send_on(
            out.iface,
            out.ip6,
            out.now,
            l6,
            r6,
            scope,
            ipv6::NH_TCP,
            64,
            |b, s, d| seg(b, s.into(), d.into()),
        )
        .is_ok(),
        (l, r) => {
            let (l4, r4) = (l.v4().unwrap_or_default(), r.v4().unwrap_or_default());
            out.iface
                .send_ip(out.now, l4, r4, PROTO_TCP, |b| seg(b, l, r))
                .is_ok()
        }
    }
}

/// [`emit`] with the whole stack at hand.
fn emit_st(
    st: &mut Stack,
    local: SockAddr,
    remote: SockAddr,
    scope: Option<usize>,
    h: Hdr,
    o: &Opts,
) -> bool {
    let now = st.now;
    let mut out = Out {
        iface: &mut st.iface,
        ip6: &mut st.ip6,
        stats: &mut st.stats,
        now,
    };
    emit(&mut out, local, remote, scope, h, o, None)
}

fn plain_hdr(seq: u32, ack: u32, flags: u8) -> Hdr {
    Hdr {
        seq,
        ack,
        flags,
        wnd: 0,
    }
}

/// RST in answer to a segment that has no connection (RFC 9293 3.10.7.1).
fn reset_for(st: &mut Stack, local: SockAddr, remote: SockAddr, seg: &Seg) {
    if seg.flags & RST != 0 {
        return;
    }
    let h = if seg.flags & ACK != 0 {
        plain_hdr(seg.ack, 0, RST)
    } else {
        plain_hdr(0, seg.seq.wrapping_add(seg.len()), RST | ACK)
    };
    // A link-local peer: answer on the link it came from.
    let scope = st.rx_link;
    emit_st(st, local, remote, scope, h, &Opts::default());
}

/// Our receive window in bytes (free buffer space, as much as the scale can express).
fn rcv_wnd(t: &Tcb, rx: &Ring) -> u32 {
    (rx.free() as u32).min(65535u32 << t.rcv_wscale)
}

/// (window field, the bytes it means) for a non-SYN segment.
fn wnd_field(t: &Tcb, rx: &Ring) -> (u16, u32) {
    let f = (rcv_wnd(t, rx) >> t.rcv_wscale).min(65535);
    (f as u16, f << t.rcv_wscale)
}

/// Options for a non-SYN segment: timestamps, and SACK blocks for our out-of-order data.
fn seg_opts(t: &Tcb, now: u64) -> Opts {
    let mut o = Opts::default();
    if t.ts_ok {
        o.ts = Some((tsval(now), t.ts_recent));
    }
    if t.sack_ok && t.nooo > 0 {
        let max = t.max_sack_blocks();
        let n = t.nooo as usize;
        // The block holding the newest segment first, then the others.
        let first = t.ooo[..n]
            .iter()
            .position(|&(a, b)| le(a, t.ooo_last) && lt(t.ooo_last, b))
            .unwrap_or(0);
        o.sack[0] = t.ooo[first];
        o.nsack = 1;
        for (k, &r) in t.ooo[..n].iter().enumerate() {
            if k != first && o.nsack < max {
                o.sack[o.nsack] = r;
                o.nsack += 1;
            }
        }
    }
    o
}

/// We just sent a segment acknowledging `rcv_nxt` with window edge `edge`.
fn acked_now(t: &mut Tcb, edge: u32) {
    t.rcv_adv = edge;
    t.last_ack_sent = t.rcv_nxt;
    t.ack_now = false;
    t.delack = None;
    t.unacked = 0;
}

/// Send whatever the connection in slot `i` has to send: SYN / SYN-ACK, retransmissions
/// in loss recovery, data within the windows, FIN, or a bare ACK if one is owed.
pub fn output(st: &mut Stack, i: usize) {
    let now = st.now;
    let Stack {
        iface,
        socks,
        stats,
        ip6,
        ..
    } = st;
    let mut out = Out {
        iface,
        ip6,
        stats,
        now,
    };
    let Some(s) = socks.get(i) else { return };
    if s.ty != SockType::Stream {
        return;
    }
    let (local, remote, scope) = (s.local, s.remote, s.scope);
    let nodelay = s.nodelay;
    let t = &mut s.tcb;
    let tx = &s.tx;
    let rx = &s.rx;
    match t.state {
        TcpState::Closed | TcpState::Listen => return,
        TcpState::SynSent | TcpState::SynReceived => {
            if t.snd_nxt == t.iss {
                let synack = t.state == TcpState::SynReceived;
                let h = Hdr {
                    seq: t.iss,
                    ack: if synack { t.rcv_nxt } else { 0 },
                    flags: if synack { SYN | ACK } else { SYN },
                    wnd: (rx.free() as u32).min(65535) as u16, // never scaled in a SYN
                };
                // Active open: offer everything; passive: answer what the peer offered.
                let o = Opts {
                    syn: true,
                    wscale: (!synack || t.rcv_wscale > 0).then_some(RCV_WSCALE),
                    sack_ok: !synack || t.sack_ok,
                    ts: (!synack || t.ts_ok).then(|| (tsval(now), t.ts_recent)),
                    ..Opts::default()
                };
                if !emit(&mut out, local, remote, scope, h, &o, None) {
                    t.kick = Some(now + KICK);
                    return;
                }
                t.snd_nxt = t.iss.wrapping_add(1);
                t.snd_max = t.snd_nxt;
                if t.retries == 0 {
                    t.rtt_probe = Some((t.snd_nxt, now));
                }
                if t.timer.is_none() {
                    t.timer = Some(now + t.rto);
                }
                if synack {
                    let edge = t.rcv_nxt.wrapping_add((rx.free() as u32).min(65535));
                    acked_now(t, edge);
                }
            }
            return;
        }
        _ => {}
    }

    // Synchronized. 1. Loss recovery: retransmit holes while the pipe has room (the
    // first one at once on entering recovery).
    if t.in_recovery && !matches!(t.state, TcpState::TimeWait) {
        let data_end = t.snd_una.wrapping_add(tx.len() as u32);
        loop {
            // The first retransmission goes at once; without SACK (NewReno) one per partial
            // ACK; with SACK, as the pipe allows.
            let forced = t.rec_first || t.nsacked == 0;
            if !forced && t.pipe() >= t.cwnd {
                break;
            }
            let Some((a, b)) = t.next_hole() else { break };
            let b = if lt(data_end, b) { data_end } else { b };
            let n = (b.wrapping_sub(a) as usize).min(t.max_data());
            let off = a.wrapping_sub(t.snd_una) as usize;
            let fin = t.fin_seq.is_some() && a.wrapping_add(n as u32) == data_end;
            if n == 0 && !fin {
                break;
            }
            let (wnd, edge) = wnd_field(t, rx);
            let o = seg_opts(t, now);
            let h = Hdr {
                seq: a,
                ack: t.rcv_nxt,
                flags: ACK | if fin { FIN } else { 0 },
                wnd,
            };
            if !emit(&mut out, local, remote, scope, h, &o, Some((tx, off, n))) {
                t.kick = Some(now + KICK);
                return;
            }
            out.stats.tcp_retrans_segs += 1;
            t.retransmits += 1;
            t.sack_retrans += 1;
            t.rec_first = false;
            t.rtt_probe = None;
            t.high_rxt = a.wrapping_add(n.max(1) as u32);
            let e = t.rcv_nxt.wrapping_add(edge);
            acked_now(t, e);
            if t.timer.is_none() {
                t.timer = Some(now + t.rto);
            }
            if t.nsacked == 0 {
                break; // NewReno: one segment per partial ACK
            }
        }
    }

    // 2. New data, then FIN.
    if !matches!(t.state, TcpState::TimeWait | TcpState::FinWait2) {
        loop {
            let data_end = t.snd_una.wrapping_add(tx.len() as u32);
            // Resending after a timeout: skip what the peer has SACKed.
            if lt(t.snd_nxt, t.snd_max)
                && let Some(end) = t.is_sacked(t.snd_nxt)
            {
                t.snd_nxt = if lt(data_end, end) { data_end } else { end };
                continue;
            }
            let in_flight = t.snd_nxt.wrapping_sub(t.snd_una);
            let outstanding = if t.in_recovery { t.pipe() } else { in_flight };
            if lt(t.snd_nxt, data_end) {
                let off = in_flight as usize;
                let unsent = data_end.wrapping_sub(t.snd_nxt) as usize;
                let by_wnd = t.snd_wnd.saturating_sub(in_flight);
                // Limited transmit (RFC 3042): each of the first two duplicate ACKs lets
                // one more new segment out, to keep the ACK clock going.
                let extra = if t.in_recovery {
                    0
                } else {
                    t.dupacks.min(2) as u32 * t.smss()
                };
                let by_cwnd = (t.cwnd + extra).saturating_sub(outstanding);
                let mut room = by_wnd.min(by_cwnd) as usize;
                if room == 0 && t.force_probe {
                    room = 1; // zero-window probe
                }
                t.force_probe = false;
                let max = t.max_data();
                let n = unsent.min(room).min(max);
                if n == 0 {
                    if t.snd_wnd == 0 && in_flight == 0 && t.timer.is_none() {
                        t.persist = true;
                        t.timer = Some(now + t.rto);
                    }
                    break;
                }
                // Sender-side silly window avoidance (RFC 9293 3.8.6.2.1): no partial
                // segment just because the window (cwnd or the peer's) has a sliver left;
                // wait for a full one while anything is in flight.
                if n < max && n < unsent && in_flight > 0 {
                    break;
                }
                // Nagle (Minshall): a small last piece waits while an earlier small segment
                // is unacknowledged, so bulk transfers never stall on their tail.
                // Never for a resend (snd_nxt below snd_max after a timeout), or with
                // nothing in flight (no ACK would come to release it).
                if n < max
                    && n == unsent
                    && in_flight > 0
                    && !lt(t.snd_nxt, t.snd_max)
                    && gt(t.snd_sml, t.snd_una)
                    && !nodelay
                    && !t.fin_queued
                {
                    break;
                }
                let fin = t.fin_queued && !t.fin_acked && n == unsent;
                let mut flags = ACK;
                if n == unsent {
                    flags |= PSH;
                }
                if fin {
                    flags |= FIN;
                }
                let (wnd, edge) = wnd_field(t, rx);
                let o = seg_opts(t, now);
                let h = Hdr {
                    seq: t.snd_nxt,
                    ack: t.rcv_nxt,
                    flags,
                    wnd,
                };
                if !emit(&mut out, local, remote, scope, h, &o, Some((tx, off, n))) {
                    t.kick = Some(now + KICK);
                    return;
                }
                let fresh = !lt(t.snd_nxt, t.snd_max);
                if !fresh {
                    out.stats.tcp_retrans_segs += 1;
                    t.retransmits += 1;
                }
                t.snd_nxt = t.snd_nxt.wrapping_add(n as u32);
                if n < max {
                    t.snd_sml = t.snd_nxt;
                }
                if fin {
                    sent_fin(t, data_end);
                }
                if gt(t.snd_nxt, t.snd_max) {
                    t.snd_max = t.snd_nxt;
                }
                if fresh && t.rtt_probe.is_none() && !t.ts_ok {
                    t.rtt_probe = Some((t.snd_nxt, now));
                }
                if t.timer.is_none() || t.persist {
                    t.persist = false;
                    t.timer = Some(now + t.rto);
                }
                let e = t.rcv_nxt.wrapping_add(edge);
                acked_now(t, e);
                if fin {
                    break;
                }
            } else if t.fin_queued && !t.fin_acked && t.snd_nxt == data_end {
                let (wnd, edge) = wnd_field(t, rx);
                let o = seg_opts(t, now);
                let h = Hdr {
                    seq: data_end,
                    ack: t.rcv_nxt,
                    flags: FIN | ACK,
                    wnd,
                };
                if !emit(&mut out, local, remote, scope, h, &o, None) {
                    t.kick = Some(now + KICK);
                    return;
                }
                t.snd_nxt = data_end.wrapping_add(1);
                sent_fin(t, data_end);
                if gt(t.snd_nxt, t.snd_max) {
                    t.snd_max = t.snd_nxt;
                }
                if t.timer.is_none() || t.persist {
                    t.persist = false;
                    t.timer = Some(now + t.rto);
                }
                let e = t.rcv_nxt.wrapping_add(edge);
                acked_now(t, e);
                break;
            } else {
                break;
            }
        }
    }
    if t.ack_now {
        let (wnd, edge) = wnd_field(t, rx);
        let o = seg_opts(t, now);
        let h = Hdr {
            seq: t.snd_nxt,
            ack: t.rcv_nxt,
            flags: ACK,
            wnd,
        };
        if emit(&mut out, local, remote, scope, h, &o, None) {
            let e = t.rcv_nxt.wrapping_add(edge);
            acked_now(t, e);
        } else {
            t.kick = Some(now + KICK);
        }
    }
}

/// Our FIN (at `seq`) has gone out.
fn sent_fin(t: &mut Tcb, seq: u32) {
    t.fin_seq = Some(seq);
    match t.state {
        TcpState::Established => t.state = TcpState::FinWait1,
        TcpState::CloseWait => t.state = TcpState::LastAck,
        _ => {}
    }
}

// ---- segment input --------------------------------------------------------------------------

struct Seg<'a> {
    sport: u16,
    dport: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    /// The raw 16-bit window field (unscaled).
    wnd: u32,
    mss: Option<u16>,
    wscale: Option<u8>,
    sack_ok: bool,
    ts: Option<(u32, u32)>,
    sack: [(u32, u32); 4],
    nsack: usize,
    data: &'a [u8],
}

impl Seg<'_> {
    /// Sequence space the segment occupies (data + SYN + FIN).
    fn len(&self) -> u32 {
        self.data.len() as u32 + (self.flags & SYN != 0) as u32 + (self.flags & FIN != 0) as u32
    }
}

fn parse(p: &[u8]) -> Option<Seg<'_>> {
    if p.len() < 20 {
        return None;
    }
    let hl = (p[12] >> 4) as usize * 4;
    if hl < 20 || hl > p.len() {
        return None;
    }
    let mut seg = Seg {
        sport: be16(p, 0),
        dport: be16(p, 2),
        seq: be32(p, 4),
        ack: be32(p, 8),
        flags: p[13],
        wnd: be16(p, 14) as u32,
        mss: None,
        wscale: None,
        sack_ok: false,
        ts: None,
        sack: [(0, 0); 4],
        nsack: 0,
        data: &p[hl..],
    };
    let mut k = 20;
    while k < hl {
        match p[k] {
            0 => break,
            1 => k += 1,
            kind => {
                let len = *p.get(k + 1)? as usize;
                if len < 2 || k + len > hl {
                    break;
                }
                match (kind, len) {
                    (2, 4) => seg.mss = Some(be16(p, k + 2)),
                    (3, 3) => seg.wscale = Some(p[k + 2].min(14)),
                    (4, 2) => seg.sack_ok = true,
                    (8, 10) => seg.ts = Some((be32(p, k + 2), be32(p, k + 6))),
                    (5, _) if (len - 2) % 8 == 0 => {
                        for b in 0..((len - 2) / 8).min(4) {
                            let o = k + 2 + 8 * b;
                            seg.sack[b] = (be32(p, o), be32(p, o + 4));
                            seg.nsack = b + 1;
                        }
                    }
                    _ => {}
                }
                k += len;
            }
        }
    }
    Some(seg)
}

fn find_conn(st: &Stack, local: SockAddr, remote: SockAddr) -> Option<usize> {
    st.socks
        .iter()
        .find(|(_, s)| {
            s.ty == SockType::Stream
                && !matches!(s.tcb.state, TcpState::Closed | TcpState::Listen)
                && s.local == local
                && s.remote == remote
        })
        .map(|(i, _)| i)
}

fn find_listener(st: &Stack, local: SockAddr) -> Option<usize> {
    let mut wild = None;
    for (i, s) in st.socks.iter() {
        if s.ty != SockType::Stream || s.tcb.state != TcpState::Listen {
            continue;
        }
        if s.local.port != local.port || !super::socket::takes(s, local.ip) {
            continue;
        }
        if s.local.ip == local.ip {
            return Some(i);
        }
        // An IPv4 wildcard beats a dual-stack IPv6 one for IPv4 (as Linux picks).
        if wild.is_none() || !s.local.ip.is_v6() {
            wild = Some(i);
        }
    }
    wild
}

/// The MSS we announce and accept at most on this family (MTU - IP - TCP headers).
fn our_mss(a: IpAddr) -> u16 {
    if a.is_v6() { OUR_MSS6 } else { OUR_MSS }
}

/// A TCP segment arrived over IPv4 (IP payload, addresses from the IP header).
pub fn input(st: &mut Stack, src: Ipv4, dst: Ipv4, p: &[u8]) {
    st.rx_link = None;
    segment(st, src.into(), dst.into(), p);
}

/// A TCP segment arrived over IPv6 on link `link` (None: loopback).
pub fn input6(st: &mut Stack, src: Ipv6, dst: Ipv6, p: &[u8], link: Option<usize>) {
    // Link-local pairs are only meaningful with their link: remember it for replies.
    st.rx_link = link.filter(|_| dst.is_link_local() || src.is_link_local());
    segment(st, IpAddr::V6(src), IpAddr::V6(dst), p);
    st.rx_link = None;
}

fn segment(st: &mut Stack, src: IpAddr, dst: IpAddr, p: &[u8]) {
    st.stats.tcp_in_segs += 1;
    if dst.is_broadcast() || dst.is_multicast() || src.is_broadcast() || src.is_multicast() {
        return;
    }
    if finish(sum(p, l4_sum(src, dst, PROTO_TCP, p.len()))) != 0 {
        st.stats.tcp_in_errs += 1;
        return;
    }
    let Some(seg) = parse(p) else {
        st.stats.tcp_in_errs += 1;
        return;
    };
    let local = SockAddr::new(dst, seg.dport);
    let remote = SockAddr::new(src, seg.sport);
    if let Some(i) = find_conn(st, local, remote) {
        match st.socks.get(i).unwrap().tcb.state {
            TcpState::SynSent => syn_sent_input(st, i, &seg),
            _ => sync_input(st, i, &seg),
        }
        park_if_time_wait(st, i);
        return;
    }
    // A pair in TIME_WAIT: tw_input answers, unless it's a new SYN that may reuse the pair
    // (the entry is then gone and the listener takes it).
    if let Some(k) = st.tw.find(local, remote)
        && !tw_input(st, k, &seg)
    {
        return;
    }
    if let Some(l) = find_listener(st, local) {
        listen_input(st, l, &seg, local, remote);
    } else {
        reset_for(st, local, remote, &seg);
    }
}

fn listen_input(st: &mut Stack, l: usize, seg: &Seg, local: SockAddr, remote: SockAddr) {
    if seg.flags & RST != 0 {
        return;
    }
    if seg.flags & ACK != 0 {
        reset_for(st, local, remote, seg);
        return;
    }
    if seg.flags & SYN == 0 {
        return;
    }
    {
        let lt_ = &st.socks.get(l).unwrap().tcb;
        if lt_.accn >= lt_.backlog {
            return; // queue full: drop the SYN, the peer retries
        }
    }
    let Some(c) = st.socks.alloc(SockType::Stream) else {
        return;
    };
    let now = st.now;
    let (nodelay, family, v6only) = {
        let ls = st.socks.get(l).unwrap();
        (ls.nodelay, ls.family, ls.v6only)
    };
    let scope = st.rx_link;
    let s = st.socks.get(c).unwrap();
    s.user_open = false;
    s.nodelay = nodelay;
    s.family = family;
    s.v6only = v6only;
    s.scope = scope;
    s.local = local;
    s.remote = remote;
    let t = &mut s.tcb;
    t.start(
        initial_seq(local, remote, now),
        seg.mss.unwrap_or(DEFAULT_MSS).min(our_mss(local.ip)),
    );
    t.state = TcpState::SynReceived;
    t.irs = seg.seq;
    t.rcv_nxt = seg.seq.wrapping_add(1);
    t.snd_wnd = seg.wnd; // a SYN's window is never scaled
    t.snd_wl1 = seg.seq;
    t.snd_wl2 = 0;
    if let Some(ws) = seg.wscale {
        t.snd_wscale = ws;
        t.rcv_wscale = RCV_WSCALE;
    }
    t.sack_ok = seg.sack_ok;
    if let Some((v, _)) = seg.ts {
        t.ts_ok = true;
        t.ts_recent = v;
        t.ts_recent_at = now;
    }
    t.cwnd = 10 * t.smss(); // RFC 6928, in whole segments
    t.parent = Some(l as u16);
    let lt_ = &mut st.socks.get(l).unwrap().tcb;
    lt_.accq[lt_.accn as usize] = c as u16;
    lt_.accn += 1;
    st.stats.tcp_passive_opens += 1;
    output(st, c);
}

fn syn_sent_input(st: &mut Stack, i: usize, seg: &Seg) {
    let now = st.now;
    let s = st.socks.get(i).unwrap();
    let t = &mut s.tcb;
    let has_ack = seg.flags & ACK != 0;
    if has_ack && (le(seg.ack, t.iss) || gt(seg.ack, t.snd_max)) {
        let (l, r) = (s.local, s.remote);
        reset_for(st, l, r, seg);
        return;
    }
    if seg.flags & RST != 0 {
        if has_ack {
            st.stats.tcp_attempt_fails += 1;
            fail(st, i, ECONNREFUSED);
        }
        return;
    }
    if seg.flags & SYN == 0 {
        return;
    }
    t.irs = seg.seq;
    t.rcv_nxt = seg.seq.wrapping_add(1);
    t.mss = seg.mss.unwrap_or(DEFAULT_MSS).min(our_mss(s.local.ip));

    // Options: on only if both sides offered them (we always offer).
    if let Some(ws) = seg.wscale {
        t.snd_wscale = ws;
        t.rcv_wscale = RCV_WSCALE;
    } else {
        t.snd_wscale = 0;
        t.rcv_wscale = 0;
    }
    t.sack_ok = seg.sack_ok;
    if let Some((v, e)) = seg.ts {
        t.ts_ok = true;
        t.ts_recent = v;
        t.ts_recent_at = now;
        if has_ack && e != 0 && t.retries == 0 {
            t.rtt_probe = None;
            t.sample_rtt((tsval(now).wrapping_sub(e) as u64).max(1) * MS);
        }
    }
    t.cwnd = 10 * t.smss(); // RFC 6928, in whole segments
    if has_ack {
        t.snd_una = seg.ack;
        t.snd_wnd = seg.wnd; // the SYN-ACK's window is not scaled
        t.snd_wl1 = seg.seq;
        t.snd_wl2 = seg.ack;
        t.state = TcpState::Established;
        t.timer = None;
        if let Some((pseq, at)) = t.rtt_probe.take()
            && ge(seg.ack, pseq)
            && t.retries == 0
        {
            t.sample_rtt(now.saturating_sub(at));
        }
        t.retries = 0;
        t.ack_now = true;
        st.wake(i);
        // Data or FIN riding on the SYN-ACK.
        let rest = Seg {
            seq: seg.seq.wrapping_add(1),
            flags: seg.flags & !SYN,
            data: seg.data,
            ..*seg
        };
        if !rest.data.is_empty() || rest.flags & FIN != 0 {
            sync_input(st, i, &rest);
            return;
        }
    } else {
        // Simultaneous open: answer with SYN-ACK.
        t.state = TcpState::SynReceived;
        t.snd_nxt = t.iss;
        t.timer = None;
    }
    output(st, i);
}

/// Segment for a connection in SYN-RECEIVED or a synchronized state (RFC 9293 3.10.7.4).
fn sync_input(st: &mut Stack, i: usize, seg: &Seg) {
    let now = st.now;
    let s = st.socks.get(i).unwrap();
    let state = s.tcb.state;
    let wnd = rcv_wnd(&s.tcb, &s.rx);
    let t = &mut s.tcb;
    let seg_wnd = seg.wnd << t.snd_wscale;

    // 0. PAWS (RFC 7323 5.3): an old timestamp means an old duplicate.
    if t.ts_ok
        && seg.flags & RST == 0
        && let Some((v, _)) = seg.ts
        && lt(v, t.ts_recent)
        && now.saturating_sub(t.ts_recent_at) < PAWS_IDLE
    {
        t.ack_now = true;
        output(st, i);
        return;
    }

    // 1. Acceptability.
    let seg_len = seg.len();
    let mut seq = seg.seq;
    let mut data = seg.data;
    let mut syn = seg.flags & SYN != 0;
    let mut fin = seg.flags & FIN != 0;
    let acceptable = match (seg_len, wnd) {
        (0, 0) => seq == t.rcv_nxt,
        (0, _) => in_window(seq, t.rcv_nxt, wnd),
        (_, 0) => false,
        _ => {
            in_window(seq, t.rcv_nxt, wnd)
                || in_window(seq.wrapping_add(seg_len - 1), t.rcv_nxt, wnd)
        }
    };
    if !acceptable {
        if wnd == 0 && seq == t.rcv_nxt {
            // Zero window: still take its ACK / RST, drop text and FIN.
            data = &[];
            fin = false;
            syn = false;
        } else {
            if seg.flags & RST == 0 {
                t.ack_now = true;
                output(st, i);
            }
            return;
        }
    }
    // RFC 7323: remember the timestamp of a segment that covers what we acked last.
    if let Some((v, _)) = seg.ts
        && t.ts_ok
        && le(seg.seq, t.last_ack_sent)
        && ge(v, t.ts_recent)
    {
        t.ts_recent = v;
        t.ts_recent_at = now;
    }
    // Trim what we already have off the front, and what does not fit off the back.
    if lt(seq, t.rcv_nxt) {
        let mut skip = t.rcv_nxt.wrapping_sub(seq) as usize;
        if syn {
            syn = false;
            skip -= 1;
            seq = seq.wrapping_add(1);
        }
        let d = skip.min(data.len());
        data = &data[d..];
        seq = seq.wrapping_add(d as u32);
        if skip > d {
            fin = false;
        }
    }
    let room = wnd.saturating_sub(seq.wrapping_sub(t.rcv_nxt)) as usize;
    if data.len() > room {
        data = &data[..room];
        fin = false;
    }

    // 2. RST (RFC 5961: only an exact match resets; in-window gets a challenge ACK).
    if seg.flags & RST != 0 {
        if seg.seq != t.rcv_nxt {
            t.ack_now = true;
            output(st, i);
            return;
        }
        match state {
            TcpState::SynReceived if t.parent.is_some() => {
                drop_child(st, i);
            }
            TcpState::SynReceived => fail(st, i, ECONNREFUSED),
            TcpState::Established
            | TcpState::FinWait1
            | TcpState::FinWait2
            | TcpState::CloseWait => {
                st.stats.tcp_estab_resets += 1;
                fail(st, i, ECONNRESET);
            }
            _ => finished(st, i),
        }
        return;
    }

    // 3. SYN inside the window.
    if syn {
        if state == TcpState::SynReceived && seg.seq == t.irs {
            t.snd_nxt = t.iss; // duplicate SYN: resend SYN-ACK
        } else {
            t.ack_now = true; // RFC 5961 challenge ACK
        }
        output(st, i);
        return;
    }

    // 4. Everything after the handshake carries an ACK.
    if seg.flags & ACK == 0 {
        return;
    }

    // 5. ACK processing.
    if state == TcpState::SynReceived {
        if lt(t.snd_una, seg.ack) && le(seg.ack, t.snd_max) {
            t.state = TcpState::Established;
            t.snd_una = seg.ack;
            t.snd_wnd = seg_wnd;
            t.snd_wl1 = seg.seq;
            t.snd_wl2 = seg.ack;
            t.timer = None;
            if let Some((_, e)) = seg.ts
                && t.ts_ok
                && e != 0
                && t.retries == 0
            {
                t.rtt_probe = None;
                t.sample_rtt((tsval(now).wrapping_sub(e) as u64).max(1) * MS);
            }
            if let Some((pseq, at)) = t.rtt_probe.take()
                && ge(seg.ack, pseq)
                && t.retries == 0
            {
                t.sample_rtt(now.saturating_sub(at));
            }
            t.retries = 0;
            if let Some(p) = t.parent {
                st.wake(p as usize);
            }
            st.wake(i);
        } else {
            let s = st.socks.get(i).unwrap();
            let (l, r) = (s.local, s.remote);
            reset_for(st, l, r, seg);
            return;
        }
    }
    let s = st.socks.get(i).unwrap();
    let t = &mut s.tcb;
    if gt(seg.ack, t.snd_max) {
        t.ack_now = true;
        output(st, i);
        return;
    }
    // SACK blocks: fold them into the scoreboard.
    let mut new_sack = false;
    if t.sack_ok {
        for &(l, r) in &seg.sack[..seg.nsack] {
            if lt(l, r) && gt(r, t.snd_una) && le(r, t.snd_max) {
                let l = if lt(l, t.snd_una) { t.snd_una } else { l };
                let before = t.sacked_bytes();
                t.sack_insert(l, r);
                new_sack |= t.sacked_bytes() != before;
            }
        }
    }
    let mut woke = false;
    if lt(t.snd_una, seg.ack) {
        let acked = seg.ack.wrapping_sub(t.snd_una);
        let data_acked = (acked as usize).min(s.tx.len());
        s.tx.consume(data_acked);
        if let Some(f) = t.fin_seq
            && ge(seg.ack, f.wrapping_add(1))
        {
            t.fin_acked = true;
        }
        t.snd_una = seg.ack;
        if lt(t.snd_nxt, t.snd_una) {
            t.snd_nxt = t.snd_una;
        }
        t.sack_prune();
        // RTT: from the timestamp echo on every ACK of new data (RFC 7323), else from
        // the one timed segment (Karn).
        if let Some((_, e)) = seg.ts
            && t.ts_ok
            && e != 0
        {
            t.rtt_probe = None;
            t.sample_rtt((tsval(now).wrapping_sub(e) as u64).max(1) * MS);
        } else if let Some((pseq, at)) = t.rtt_probe
            && ge(seg.ack, pseq)
        {
            t.rtt_probe = None;
            t.sample_rtt(now.saturating_sub(at));
        }
        t.retries = 0;
        // New data got through: drop the timeout backoff (as BSD does). Without it, Karn's
        // rule (no samples from resent data) can leave the RTO doubled for good under loss.
        t.rto = t.rto_base;
        let mss = t.smss();
        if t.in_recovery {
            if ge(t.snd_una, t.recover) {
                t.in_recovery = false; // full ACK: recovery is over
                t.cwnd = t.ssthresh;
                t.nsacked = 0;
            }
            // Partial ACK: stay in recovery, cwnd unchanged (pipe decides).
        } else if t.cwnd < t.ssthresh {
            t.cwnd = t.cwnd.saturating_add(acked.min(mss));
        } else {
            t.cwnd = t.cwnd.saturating_add((mss * mss / t.cwnd.max(1)).max(1));
        }
        t.dupacks = 0;
        t.persist = false;
        t.timer = if t.snd_una == t.snd_max {
            None
        } else {
            Some(now + t.rto)
        };
        woke = true;
    } else if seg.ack == t.snd_una
        && seg.data.is_empty()
        && seg.flags & FIN == 0
        && (seg_wnd == t.snd_wnd || new_sack)
        && t.snd_una != t.snd_max
    {
        t.dupacks = t.dupacks.saturating_add(1);
    }
    // Enter loss recovery: three duplicate ACKs, or (with SACK) more than two segments'
    // worth SACKed above the first hole (RFC 6675 IsLost).
    // Early retransmit (RFC 5827): with fewer than 4 segments in flight, there can't be
    // 3 duplicate ACKs, so the threshold is the flight minus one.
    let mss = t.smss();
    let segs = t.snd_max.wrapping_sub(t.snd_una).div_ceil(mss);
    let thresh = if segs < 4 {
        segs.saturating_sub(1).max(1)
    } else {
        3
    };
    if !t.in_recovery
        && t.snd_una != t.snd_max
        && (t.dupacks as u32 >= thresh || (t.sack_ok && t.sacked_bytes() >= thresh * mss))
    {
        let flight = t.snd_max.wrapping_sub(t.snd_una);
        t.ssthresh = (flight / 2).max(2 * mss);
        t.cwnd = t.ssthresh;
        t.in_recovery = true;
        t.recover = t.snd_max;
        t.high_rxt = t.snd_una;
        t.rec_first = true;
    }
    // Window update (RFC 9293: only from a segment newer than the last update).
    if lt(t.snd_wl1, seg.seq) || (t.snd_wl1 == seg.seq && le(t.snd_wl2, seg.ack)) {
        if seg_wnd > t.snd_wnd {
            woke = true;
        }
        t.snd_wnd = seg_wnd;
        t.snd_wl1 = seg.seq;
        t.snd_wl2 = seg.ack;
    }
    if t.snd_wnd == 0 && seg.ack == t.snd_una {
        t.retries = 0; // the peer is alive, just full
    }
    if woke {
        st.wake(i);
    }
    let s = st.socks.get(i).unwrap();
    let t = &mut s.tcb;
    match t.state {
        TcpState::FinWait1 if t.fin_acked => {
            t.state = TcpState::FinWait2;
            t.timer = None;
            if !s.user_open {
                t.deadline = Some(now + FIN_TIMEOUT);
            }
            st.wake(i);
        }
        TcpState::Closing if t.fin_acked => {
            enter_time_wait(t, now);
            st.wake(i);
        }
        TcpState::LastAck if t.fin_acked => {
            finished(st, i);
            return;
        }
        _ => {}
    }

    // 6/7. Segment text.
    let s = st.socks.get(i).unwrap();
    let t = &mut s.tcb;
    let orphan = !s.user_open && t.parent.is_none();
    if !data.is_empty() {
        match t.state {
            TcpState::Established | TcpState::FinWait1 | TcpState::FinWait2 => {
                if orphan {
                    // Data for a socket nobody will read: abort (as Linux does).
                    let h = plain_hdr(t.snd_nxt, 0, RST);
                    let (l, r, sc) = (s.local, s.remote, s.scope);
                    emit_st(st, l, r, sc, h, &Opts::default());
                    finished(st, i);
                    return;
                }
                if let Some((_, e)) = seg.ts
                    && e != 0
                {
                    // Receiver-side RTT, for auto-sizing the receive buffer.
                    let r = (tsval(now).wrapping_sub(e) as u64).max(1) * MS;
                    t.rcv_rtt = if t.rcv_rtt == 0 {
                        r
                    } else {
                        (7 * t.rcv_rtt + r) / 8
                    };
                }
                if seq == t.rcv_nxt {
                    let had_ooo = t.nooo > 0;
                    let n = if s.shut_rd {
                        data.len() // read side shut: acknowledge and discard
                    } else {
                        s.rx.push(data)
                    };
                    t.rcv_nxt = t.rcv_nxt.wrapping_add(n as u32);
                    if n < data.len() {
                        fin = false;
                    }
                    if ooo_advance(t, &mut s.rx) {
                        fin = false; // the FIN sat before data we already had: wait for it
                    }
                    t.unacked = t.unacked.saturating_add(n as u32);
                    // Delayed ACK, except: quick-ACK mode, a hole was (being) filled, two full segments
                    // are waiting, or the peer is pushing a short final piece.
                    // "Every second segment" counted in bytes as Linux does: more than one
                    // full segment's worth unacknowledged (a full one plus a small one
                    // counts, or Nagle on the other side would wait for our delayed ACK).
                    if had_ooo || t.unacked > t.mss as u32 || t.quickack > 0 {
                        t.quickack = t.quickack.saturating_sub(1);
                        t.ack_now = true;
                    } else if t.delack.is_none() {
                        t.delack = Some(now + DELACK);
                    }
                    st.wake(i);
                } else {
                    // Out of order: keep it in the ring past the readable end (if there is
                    // room in the gap list), and ACK at once (with SACK blocks) so the
                    // peer can fill the hole.
                    fin = false;
                    if !s.shut_rd {
                        let off = seq.wrapping_sub(t.rcv_nxt) as usize;
                        let n = s.rx.write_at(off, data);
                        if n > 0 {
                            ooo_insert(t, seq, seq.wrapping_add(n as u32));
                            t.ooo_last = seq;
                        }
                    }
                    t.ack_now = true;
                }
            }
            _ => {}
        }
    }

    // 8. FIN.
    let s = st.socks.get(i).unwrap();
    let t = &mut s.tcb;
    if fin && seq.wrapping_add(data.len() as u32) == t.rcv_nxt {
        t.rcv_nxt = t.rcv_nxt.wrapping_add(1);
        t.peer_fin = true;
        t.ack_now = true;
        match t.state {
            TcpState::SynReceived | TcpState::Established => t.state = TcpState::CloseWait,
            TcpState::FinWait1 => {
                if t.fin_acked {
                    enter_time_wait(t, now);
                } else {
                    t.state = TcpState::Closing;
                }
            }
            TcpState::FinWait2 => enter_time_wait(t, now),
            TcpState::TimeWait => t.deadline = Some(now + 2 * MSL),
            _ => {}
        }
        st.wake(i);
    }
    output(st, i);
}

/// Remember out-of-order range [a, b) (merging with overlapping or touching ones). If the
/// list is full the range is forgotten (the data is resent later anyway).
fn ooo_insert(t: &mut Tcb, mut a: u32, mut b: u32) {
    let base = t.rcv_nxt;
    let rel = |x: u32| x.wrapping_sub(base);
    let mut k = 0;
    while k < t.nooo as usize {
        let (s, e) = t.ooo[k];
        if rel(s) <= rel(b) && rel(a) <= rel(e) {
            if rel(s) < rel(a) {
                a = s;
            }
            if rel(e) > rel(b) {
                b = e;
            }
            t.ooo[k] = t.ooo[t.nooo as usize - 1];
            t.nooo -= 1;
        } else {
            k += 1;
        }
    }
    if (t.nooo as usize) < MAX_OOO {
        t.ooo[t.nooo as usize] = (a, b);
        t.nooo += 1;
    }
}

/// rcv_nxt moved: make any stored out-of-order data it now reaches readable. Returns true
/// if anything was merged.
fn ooo_advance(t: &mut Tcb, rx: &mut Ring) -> bool {
    let mut merged = false;
    loop {
        let base = t.rcv_nxt;
        let hit = t.ooo[..t.nooo as usize]
            .iter()
            .position(|&(s, _)| le(s, base));
        let Some(k) = hit else { return merged };
        let e = t.ooo[k].1;
        if gt(e, base) {
            let extra = e.wrapping_sub(base);
            rx.commit(extra as usize);
            t.rcv_nxt = e;
            merged = true;
        }
        t.ooo[k] = t.ooo[t.nooo as usize - 1];
        t.nooo -= 1;
    }
}

fn enter_time_wait(t: &mut Tcb, now: u64) {
    t.state = TcpState::TimeWait;
    t.timer = None;
    t.delack = None;
    t.deadline = Some(now + 2 * MSL);
}

// ---- TIME_WAIT ----------------------------------------------------------------------------------

/// A connection in TIME_WAIT, without its socket: enough to re-ACK a retransmitted FIN,
/// refuse old duplicates and decide whether a new SYN may reuse the pair.
#[derive(Clone, Copy)]
pub struct TimeWait {
    pub local: SockAddr,
    pub remote: SockAddr,
    pub snd_nxt: u32,
    pub rcv_nxt: u32,
    pub ts_ok: bool,
    pub ts_recent: u32,
    pub rcv_wscale: u8,
    pub deadline: u64,
    /// It was an AF_INET6 socket's (listed in /proc/net/tcp6, IPv4 peers as ::ffff:).
    pub v6_sock: bool,
}

pub struct TwTable {
    pub entries: alloc::vec::Vec<TimeWait>,
}

impl TwTable {
    pub fn new() -> TwTable {
        TwTable {
            entries: alloc::vec::Vec::new(),
        }
    }
    pub fn find(&self, local: SockAddr, remote: SockAddr) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.local == local && e.remote == remote)
    }
    pub fn remove(&mut self, k: usize) {
        self.entries.swap_remove(k);
    }
    fn add(&mut self, e: TimeWait) {
        if self.entries.len() >= TW_MAX
            && let Some(k) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.deadline)
                .map(|(k, _)| k)
        {
            self.entries.swap_remove(k);
        }
        self.entries.push(e);
    }
    /// The earliest expiry, for the net thread's timer.
    pub fn next_deadline(&self) -> Option<u64> {
        self.entries.iter().map(|e| e.deadline).min()
    }
    fn expire(&mut self, now: u64) {
        self.entries.retain(|e| e.deadline > now);
    }
}

/// A socket the user no longer holds reached TIME_WAIT: move it to the table and free the
/// slot (and its buffers).
fn park_if_time_wait(st: &mut Stack, i: usize) {
    let Some(s) = st.socks.get(i) else { return };
    if s.ty != SockType::Stream || s.tcb.state != TcpState::TimeWait || s.user_open {
        return;
    }
    let t = &s.tcb;
    let e = TimeWait {
        local: s.local,
        remote: s.remote,
        snd_nxt: t.snd_nxt,
        rcv_nxt: t.rcv_nxt,
        ts_ok: t.ts_ok,
        ts_recent: t.ts_recent,
        rcv_wscale: t.rcv_wscale,
        deadline: t.deadline.unwrap_or(st.now + 2 * MSL),
        v6_sock: s.family == super::socket::Family::V6,
    };
    st.tw.add(e);
    finished(st, i);
}

/// A segment for a pair in TIME_WAIT. Returns true if it is a SYN that may start a new
/// connection (the entry has been removed; the caller hands it to the listener).
fn tw_input(st: &mut Stack, k: usize, seg: &Seg) -> bool {
    let now = st.now;
    let e = st.tw.entries[k];
    if seg.flags & RST != 0 {
        return false; // RFC 1337: TIME_WAIT ignores RSTs
    }
    if seg.flags & SYN != 0 && seg.flags & ACK == 0 {
        // RFC 6191: a new incarnation is fine if it starts above what we saw, or (with
        // timestamps) is newer.
        let newer = match (e.ts_ok, seg.ts) {
            (true, Some((v, _))) => gt(v, e.ts_recent),
            _ => gt(seg.seq, e.rcv_nxt),
        };
        if newer {
            st.tw.remove(k);
            return true;
        }
    }
    if seg.flags & FIN != 0 {
        st.tw.entries[k].deadline = now + 2 * MSL; // the peer lost our ACK of its FIN
    }
    let o = Opts {
        ts: e.ts_ok.then_some((tsval(now), e.ts_recent)),
        ..Opts::default()
    };
    let h = Hdr {
        seq: e.snd_nxt,
        ack: e.rcv_nxt,
        flags: ACK,
        wnd: 0,
    };
    let scope = st.rx_link;
    emit_st(st, e.local, e.remote, scope, h, &o);
    false
}

// ---- endings ----------------------------------------------------------------------------------

/// Remove child `c` from its listener's queue.
fn unqueue(st: &mut Stack, c: usize) {
    let Some(p) = st.socks.get(c).and_then(|s| s.tcb.parent) else {
        return;
    };
    if let Some(l) = st.socks.get(p as usize) {
        let t = &mut l.tcb;
        let n = t.accn as usize;
        if let Some(k) = t.accq[..n].iter().position(|&x| x as usize == c) {
            t.accq.copy_within(k + 1..n, k);
            t.accn -= 1;
        }
    }
    if let Some(s) = st.socks.get(c) {
        s.tcb.parent = None;
    }
}

/// A not-yet-accepted connection died: forget it.
fn drop_child(st: &mut Stack, c: usize) {
    unqueue(st, c);
    st.socks.free(c);
}

/// The connection ended normally (or was reset in a closing state). Orphans are freed;
/// a socket the user still holds stays CLOSED (recv gives 0).
fn finished(st: &mut Stack, i: usize) {
    let Some(s) = st.socks.get(i) else { return };
    s.tcb.state = TcpState::Closed;
    s.tcb.timer = None;
    s.tcb.deadline = None;
    s.tcb.delack = None;
    s.tcb.kick = None;
    if s.tcb.parent.is_some() {
        drop_child(st, i);
    } else if !s.user_open {
        st.socks.free(i);
    } else {
        st.wake(i);
    }
}

/// The connection failed with `e` (refused, reset, timed out).
fn fail(st: &mut Stack, i: usize, e: Errno) {
    let Some(s) = st.socks.get(i) else { return };
    if s.tcb.state == TcpState::SynSent {
        s.tcb.used = false; // a failed connect may be retried on the same socket (Linux)
    }
    s.error = Some(e);
    s.tx.clear();
    finished(st, i);
}

fn send_rst_now(st: &mut Stack, i: usize) {
    let now = st.now;
    let Some(s) = st.socks.get(i) else { return };
    let h = plain_hdr(s.tcb.snd_nxt, s.tcb.rcv_nxt, RST | ACK);
    let (l, r, sc) = (s.local, s.remote, s.scope);
    let _ = now;
    emit_st(st, l, r, sc, h, &Opts::default());
}

// ---- timers -------------------------------------------------------------------------------------

/// Run TCP timers (from the net thread).
pub fn tick(st: &mut Stack) {
    let now = st.now;
    st.tw.expire(now);
    for i in 0..super::socket::MAX_SOCKETS {
        let Some(s) = st.socks.get(i) else { continue };
        if s.ty != SockType::Stream {
            continue;
        }
        let (state, user_open) = (s.tcb.state, s.user_open);
        if let Some(d) = s.tcb.deadline
            && now >= d
            && (state == TcpState::TimeWait || (state == TcpState::FinWait2 && !user_open))
        {
            finished(st, i);
            continue;
        }
        if state == TcpState::TimeWait && !user_open {
            park_if_time_wait(st, i);
            continue;
        }
        let t = &mut s.tcb;
        let mut send = false;
        if let Some(d) = t.delack
            && now >= d
        {
            t.delack = None;
            t.ack_now = true;
            send = true;
        }
        if let Some(d) = t.kick
            && now >= d
        {
            t.kick = None;
            send = true;
        }
        if t.timer.is_some_and(|d| now >= d) {
            on_timer(st, i);
        } else if send {
            output(st, i);
        }
    }
}

/// One line of a connection's sender and receiver state, for test failures.
pub fn debug_line(t: &Tcb, txlen: usize, now: u64) -> alloc::string::String {
    alloc::format!(
        "{} una={} nxt={} max={} txlen={} wnd={} cwnd={} ssth={} pipe={} rec={} nsacked={} high_rxt={} sml={} timer={:?} kick={:?} delack={:?} rto={} rcv_nxt={} nooo={} dupacks={}",
        t.state.name(),
        t.snd_una.wrapping_sub(t.iss),
        t.snd_nxt.wrapping_sub(t.iss),
        t.snd_max.wrapping_sub(t.iss),
        txlen,
        t.snd_wnd,
        t.cwnd,
        t.ssthresh,
        t.pipe(),
        t.in_recovery,
        t.nsacked,
        t.high_rxt.wrapping_sub(t.iss),
        t.snd_sml.wrapping_sub(t.iss),
        t.timer.map(|d| d as i64 - now as i64),
        t.kick.map(|d| d as i64 - now as i64),
        t.delack.map(|d| d as i64 - now as i64),
        t.rto / MS,
        t.rcv_nxt.wrapping_sub(t.irs),
        t.nooo,
        t.dupacks
    )
}

/// The earliest TCP timer of socket `t` (retransmission/persist, TIME_WAIT/FIN_WAIT_2,
/// delayed ACK, send retry).
pub fn next_timer(t: &Tcb) -> Option<u64> {
    [t.timer, t.deadline, t.delack, t.kick]
        .into_iter()
        .flatten()
        .min()
}

fn on_timer(st: &mut Stack, i: usize) {
    let now = st.now;
    let s = st.socks.get(i).unwrap();
    let t = &mut s.tcb;
    t.timer = None;
    if t.persist {
        t.persist = false;
        t.force_probe = true;
        t.rto = (t.rto * 2).min(RTO_MAX);
        output(st, i);
        return;
    }
    t.retries += 1;
    t.timeouts += 1;
    t.max_rto_ms = t.max_rto_ms.max((t.rto / MS) as u32);
    let handshake = matches!(t.state, TcpState::SynSent | TcpState::SynReceived);
    let limit = if handshake { SYN_RETRIES } else { DATA_RETRIES };
    if t.retries > limit {
        match t.state {
            TcpState::SynSent => {
                st.stats.tcp_attempt_fails += 1;
                fail(st, i, ETIMEDOUT);
            }
            TcpState::SynReceived if t.parent.is_some() => drop_child(st, i),
            _ => {
                send_rst_now(st, i);
                fail(st, i, ETIMEDOUT);
            }
        }
        return;
    }
    let s = st.socks.get(i).unwrap();
    let t = &mut s.tcb;
    t.rto = (t.rto * 2).min(RTO_MAX);
    t.rtt_probe = None;
    if handshake {
        st.stats.tcp_retrans_segs += 1;
        t.retransmits += 1;
        t.snd_nxt = t.iss;
    } else {
        // Timeout: back to slow start from snd_una. The first time, keep the SACK
        // scoreboard so the resend skips what the peer has; a second timeout in a row
        // forgets it (the receiver may have dropped what it SACKed, RFC 2018 8).
        let flight = t.snd_max.wrapping_sub(t.snd_una);
        let mss = t.smss();
        t.ssthresh = (flight / 2).max(2 * mss);
        t.cwnd = mss;
        t.dupacks = 0;
        t.in_recovery = false;
        if t.retries >= 2 {
            t.nsacked = 0;
        }
        t.snd_nxt = t.snd_una;
    }
    let _ = now;
    output(st, i);
}

// ---- user calls (from socket.rs) -------------------------------------------------------------

/// Active open: send the SYN.
pub fn connect(st: &mut Stack, i: usize, remote: SockAddr) {
    let now = st.now;
    let s = st.socks.get(i).unwrap();
    s.remote = remote;
    let local = s.local;
    // Our own TIME_WAIT for this pair: a new incarnation reuses it (our ISN is clock-based,
    // so above the old sequence space), as Linux does with timestamps.
    if let Some(k) = st.tw.find(local, remote) {
        st.tw.remove(k);
    }
    let s = st.socks.get(i).unwrap();
    let iss = initial_seq(local, remote, now);
    let t = &mut s.tcb;
    t.start(iss, our_mss(local.ip));
    t.state = TcpState::SynSent;
    t.connect_pending = true;
    st.stats.tcp_active_opens += 1;
    output(st, i);
}

pub fn send(st: &mut Stack, i: usize, buf: &[u8]) -> Result<usize> {
    let s = st.socks.get(i).unwrap();
    if let Some(e) = s.error.take() {
        return Err(e);
    }
    if s.shut_wr {
        return Err(EPIPE);
    }
    match s.tcb.state {
        TcpState::Established | TcpState::CloseWait => {
            if buf.is_empty() {
                return Ok(0);
            }
            // Auto-size: grow the send buffer when it, not the network, is the limit.
            if !s.sndbuf_locked && s.tx.free() < buf.len() && s.tx.cap() < MAX_BUF {
                let t = &s.tcb;
                let can = (t.cwnd.min(t.snd_wnd) as usize).saturating_mul(2);
                if can > s.tx.cap() {
                    let new = (s.tx.cap() * 2).min(MAX_BUF);
                    let _ = s.tx.grow(new);
                }
            }
            let n = s.tx.push(buf);
            if n == 0 {
                return Err(EAGAIN);
            }
            output(st, i);
            Ok(n)
        }
        TcpState::SynSent | TcpState::SynReceived => Err(EAGAIN),
        TcpState::Closed if s.tcb.used => Err(EPIPE),
        TcpState::Closed | TcpState::Listen => Err(ENOTCONN),
        _ => Err(EPIPE),
    }
}

pub fn recv(st: &mut Stack, i: usize, buf: &mut [u8]) -> Result<usize> {
    let now = st.now;
    let s = st.socks.get(i).unwrap();
    if buf.is_empty() {
        return Ok(0);
    }
    if !s.rx.is_empty() {
        let n = s.rx.pop(buf);
        let t = &mut s.tcb;
        // Auto-size: if the user drains more than half the buffer per round trip, the
        // window (= the buffer) is what limits us: double it.
        if !s.rcvbuf_locked && t.state.synchronized() {
            t.rcv_copied += n;
            let rtt = if t.rcv_rtt > 0 {
                t.rcv_rtt
            } else if t.srtt > 0 {
                t.srtt
            } else {
                20 * MS
            };
            if now.saturating_sub(t.rcv_mark) >= rtt.max(MS) {
                if t.rcv_copied * 2 >= s.rx.cap() && s.rx.cap() < MAX_BUF {
                    let new = (s.rx.cap() * 2).min(MAX_BUF);
                    let _ = s.rx.grow(new);
                }
                t.rcv_copied = 0;
                t.rcv_mark = now;
            }
        }
        // Window update once the window has opened by a useful amount.
        if t.state.synchronized() {
            let edge = t.rcv_nxt.wrapping_add(rcv_wnd(t, &s.rx));
            let grow = edge.wrapping_sub(t.rcv_adv);
            if (grow as i32) > 0 && grow as usize >= (2 * t.mss as usize).min(s.rx.cap() / 2) {
                t.ack_now = true;
                output(st, i);
            }
        }
        return Ok(n);
    }
    if s.shut_rd {
        return Ok(0);
    }
    if let Some(e) = s.error.take() {
        return Err(e);
    }
    if s.tcb.peer_fin {
        return Ok(0);
    }
    match s.tcb.state {
        TcpState::Closed if s.tcb.used => Ok(0),
        TcpState::Closed | TcpState::Listen => Err(ENOTCONN),
        _ => Err(EAGAIN),
    }
}

/// shutdown(SHUT_WR): FIN after the queued data.
pub fn shutdown_write(st: &mut Stack, i: usize) {
    let s = st.socks.get(i).unwrap();
    if s.tcb.state.can_send() {
        s.tcb.fin_queued = true;
        output(st, i);
    }
}

/// The user closed the socket.
pub fn close(st: &mut Stack, i: usize) {
    let now = st.now;
    let s = st.socks.get(i).unwrap();
    match s.tcb.state {
        TcpState::Listen => {
            let kids = s.tcb.accq;
            let n = s.tcb.accn as usize;
            for &c in &kids[..n] {
                send_rst_now(st, c as usize);
                st.socks.free(c as usize);
            }
            st.socks.free(i);
        }
        TcpState::Closed | TcpState::SynSent => st.socks.free(i),
        TcpState::SynReceived => {
            send_rst_now(st, i);
            st.socks.free(i);
        }
        TcpState::Established | TcpState::CloseWait => {
            if !s.rx.is_empty() {
                // Unread data: RST instead of FIN (RFC 2525, Linux).
                send_rst_now(st, i);
                st.socks.free(i);
                return;
            }
            s.user_open = false;
            s.shut_wr = true;
            s.tcb.fin_queued = true;
            output(st, i);
        }
        TcpState::FinWait2 => {
            s.user_open = false;
            s.tcb.deadline = Some(now + FIN_TIMEOUT);
        }
        TcpState::TimeWait => {
            s.user_open = false;
            park_if_time_wait(st, i);
        }
        _ => s.user_open = false,
    }
}

/// Next accepted-and-ready child of listener `l`, removed from its queue.
pub fn accept_ready(st: &mut Stack, l: usize) -> Option<usize> {
    let t = &st.socks.get(l)?.tcb;
    let n = t.accn as usize;
    let q = t.accq;
    for &c in &q[..n] {
        let c = c as usize;
        let ready = st
            .socks
            .get(c)
            .is_some_and(|s| s.tcb.state.synchronized() || s.tcb.state == TcpState::Closed);
        if ready {
            unqueue(st, c);
            return Some(c);
        }
    }
    None
}

/// poll(2) bits for a TCP socket.
pub fn poll(st: &mut Stack, i: usize) -> u32 {
    let s = st.socks.get(i).unwrap();
    if s.tcb.state == TcpState::Listen {
        let (q, n) = (s.tcb.accq, s.tcb.accn as usize);
        let ready = q[..n].iter().any(|&c| {
            st.socks
                .get(c as usize)
                .is_some_and(|k| k.tcb.state.synchronized())
        });
        return if ready { POLLIN | POLLRDNORM } else { 0 };
    }
    let t = &s.tcb;
    let mut m = 0;
    if !s.rx.is_empty() || t.peer_fin || s.shut_rd {
        m |= POLLIN | POLLRDNORM;
    }
    if t.peer_fin || s.shut_rd {
        m |= POLLRDHUP;
    }
    if s.error.is_some() {
        m |= POLLERR;
    }
    if t.state.can_send() && !s.shut_wr && s.tx.free() > 0 {
        m |= POLLOUT | POLLWRNORM;
    }
    if t.state == TcpState::Closed {
        m |= POLLHUP;
        if t.used {
            m |= POLLIN | POLLRDNORM;
        }
    }
    if s.shut_wr && t.peer_fin {
        m |= POLLHUP;
    }
    m
}
