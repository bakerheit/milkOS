//! TCP (RFC 793 / 9293, with RFC 6298 retransmission timing, RFC 5681 slow start +
//! congestion avoidance + fast retransmit, RFC 5961 challenge ACKs, RFC 879 MSS option).
//!
//! Deliberate simplifications (correct, just not fast): no window scaling (16 KiB buffers
//! fit in 16 bits), no SACK, no timestamps, no delayed ACKs (every data segment is ACKed at
//! once), no Nagle, and out-of-order segments are dropped (the peer retransmits). On a
//! retransmission timeout we go back to `snd_una` and resend from there (go-back-N).
//!
//! Each connection's send buffer holds the bytes from `snd_una` on (unacknowledged and
//! unsent); its receive buffer holds what the user has not read, and its free space is the
//! window we advertise. Both are fixed rings from the socket pool (no allocation here).

use super::Stack;
use super::Stats;
use super::errno::*;
use super::iface::{Iface, PROTO_TCP};
use super::ring::Ring;
use super::socket::{
    POLLERR, POLLHUP, POLLIN, POLLOUT, POLLRDHUP, POLLRDNORM, POLLWRNORM, SockType,
};
use super::wire::{Ipv4, SockAddr, be16, be32, finish, pseudo_sum, put16, put32, sum};
use crate::fs::errno::{Errno, Result};

pub const MAX_BACKLOG: usize = 16;
/// MSS we announce and use at most (1500 MTU - 40).
pub const OUR_MSS: u16 = 1460;
/// MSS assumed when the peer sends no option (RFC 9293).
const DEFAULT_MSS: u16 = 536;

const MS: u64 = 1_000_000;
const RTO_INIT: u64 = 1000 * MS;
const RTO_MIN: u64 = 200 * MS;
const RTO_MAX: u64 = 60_000 * MS;
/// SYN / SYN-ACK retransmissions before giving up (1+2+4+8+16 s, then fail at ~31 s+32).
const SYN_RETRIES: u8 = 5;
/// Data retransmissions before the connection is aborted with ETIMEDOUT.
const DATA_RETRIES: u8 = 10;
/// Maximum segment lifetime; TIME_WAIT lasts 2 * MSL. Short (Linux: 60 s total) to keep
/// the 32-slot table free; fine on a LAN / slirp.
const MSL: u64 = 1000 * MS;
/// An orphaned connection may sit in FIN_WAIT_2 this long (Linux tcp_fin_timeout).
const FIN_TIMEOUT: u64 = 30_000 * MS;

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

/// Transmission control block.
pub struct Tcb {
    pub state: TcpState,
    /// Has been connected (or accepted) once: a closed socket is not reusable.
    pub used: bool,
    pub iss: u32,
    pub snd_una: u32,
    pub snd_nxt: u32,
    /// Highest sequence number sent so far (snd_nxt rewinds on retransmission).
    pub snd_max: u32,
    pub snd_wnd: u32,
    snd_wl1: u32,
    snd_wl2: u32,
    pub irs: u32,
    pub rcv_nxt: u32,
    /// Right edge of the window we last advertised.
    rcv_adv: u32,
    /// Send MSS (min of the peer's option and ours).
    pub mss: u16,
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
    /// (sequence number that acks the timed segment, send time). Karn: never a resend.
    rtt_probe: Option<(u32, u64)>,
    /// Retransmission (or persist) deadline.
    pub timer: Option<u64>,
    persist: bool,
    force_probe: bool,
    pub retries: u8,
    /// TIME_WAIT end, or the orphan FIN_WAIT_2 timeout.
    pub deadline: Option<u64>,
    pub cwnd: u32,
    ssthresh: u32,
    dupacks: u8,
    ack_now: bool,
    /// A connection not yet accepted: slot of its listener.
    pub parent: Option<u16>,
    accq: [u16; MAX_BACKLOG],
    accn: u8,
    pub backlog: u8,
    pub connect_pending: bool,
    pub retransmits: u32,
    /// Out-of-order data already written into the receive ring past its end, as absolute
    /// sequence ranges [start, end).
    ooo: [(u32, u32); MAX_OOO],
    nooo: u8,
}

const MAX_OOO: usize = 8;

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
            cwnd: 0,
            ssthresh: u32::MAX,
            dupacks: 0,
            ack_now: false,
            parent: None,
            accq: [0; MAX_BACKLOG],
            accn: 0,
            backlog: 0,
            connect_pending: false,
            retransmits: 0,
            ooo: [(0, 0); MAX_OOO],
            nooo: 0,
        }
    }

    /// Children (in handshake or ready) queued on this listener.
    pub fn queued(&self) -> &[u16] {
        &self.accq[..self.accn as usize]
    }

    fn start(&mut self, iss: u32, mss: u16) {
        self.used = true;
        self.iss = iss;
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

fn initial_seq(local: SockAddr, remote: SockAddr, now: u64) -> u32 {
    // RFC 6528 shape: per-connection offset + a 4 µs clock.
    let mut h: u32 = 0x811c_9dc5;
    let (lp, rp) = (local.port.to_be_bytes(), remote.port.to_be_bytes());
    let bytes = local.ip.0.iter().chain(&remote.ip.0).chain(&lp).chain(&rp);
    for &b in bytes {
        h = (h ^ b as u32).wrapping_mul(16_777_619);
    }
    h.wrapping_add((now / 4000) as u32)
}

// ---- segment output -------------------------------------------------------------------------

struct Hdr {
    seq: u32,
    ack: u32,
    flags: u8,
    wnd: u16,
    mss: bool,
}

/// Build and send one segment. `data`: (ring, offset from its head, length).
fn emit(
    iface: &mut Iface,
    stats: &mut Stats,
    now: u64,
    local: SockAddr,
    remote: SockAddr,
    h: Hdr,
    data: Option<(&Ring, usize, usize)>,
) {
    let hl = if h.mss { 24 } else { 20 };
    stats.tcp_out_segs += 1;
    stats.ip_out_requests += 1;
    if h.flags & RST != 0 {
        stats.tcp_out_rsts += 1;
    }
    let _ = iface.send_ip(now, local.ip, remote.ip, PROTO_TCP, |b| {
        put16(b, 0, local.port);
        put16(b, 2, remote.port);
        put32(b, 4, h.seq);
        put32(b, 8, h.ack);
        b[12] = ((hl / 4) << 4) as u8;
        b[13] = h.flags;
        put16(b, 14, h.wnd);
        put16(b, 16, 0);
        put16(b, 18, 0);
        if h.mss {
            b[20] = 2;
            b[21] = 4;
            put16(b, 22, OUR_MSS);
        }
        let mut n = hl;
        if let Some((ring, off, len)) = data {
            n += ring.peek_at(off, &mut b[hl..hl + len]);
        }
        let c = finish(sum(&b[..n], pseudo_sum(local.ip, remote.ip, PROTO_TCP, n)));
        put16(b, 16, c);
        n
    });
}

/// RST in answer to a segment that has no connection (RFC 9293 3.10.7.1).
fn reset_for(st: &mut Stack, local: SockAddr, remote: SockAddr, seg: &Seg) {
    if seg.flags & RST != 0 {
        return;
    }
    let h = if seg.flags & ACK != 0 {
        Hdr {
            seq: seg.ack,
            ack: 0,
            flags: RST,
            wnd: 0,
            mss: false,
        }
    } else {
        Hdr {
            seq: 0,
            ack: seg.seq.wrapping_add(seg.len()),
            flags: RST | ACK,
            wnd: 0,
            mss: false,
        }
    };
    let now = st.now;
    emit(&mut st.iface, &mut st.stats, now, local, remote, h, None);
}

/// Our receive window: free buffer space, capped to what 16 bits can say.
fn rcv_wnd(rx: &Ring) -> u32 {
    rx.free().min(65535) as u32
}

/// Send whatever the connection in slot `i` has to send: SYN / SYN-ACK, data within the
/// window, FIN, or a bare ACK if one is owed.
pub fn output(st: &mut Stack, i: usize) {
    let now = st.now;
    let Stack {
        iface,
        socks,
        stats,
        ..
    } = st;
    let Some(s) = socks.get(i) else { return };
    if s.ty != SockType::Stream {
        return;
    }
    let (local, remote) = (s.local, s.remote);
    let t = &mut s.tcb;
    let tx = &s.tx;
    let wnd = rcv_wnd(&s.rx);
    let wnd16 = wnd as u16;
    match t.state {
        TcpState::Closed | TcpState::Listen => return,
        TcpState::SynSent | TcpState::SynReceived => {
            if t.snd_nxt == t.iss {
                let synack = t.state == TcpState::SynReceived;
                let h = Hdr {
                    seq: t.iss,
                    ack: if synack { t.rcv_nxt } else { 0 },
                    flags: if synack { SYN | ACK } else { SYN },
                    wnd: wnd16,
                    mss: true,
                };
                emit(iface, stats, now, local, remote, h, None);
                t.snd_nxt = t.iss.wrapping_add(1);
                t.snd_max = t.snd_nxt;
                if t.retries == 0 {
                    t.rtt_probe = Some((t.snd_nxt, now));
                }
                if t.timer.is_none() {
                    t.timer = Some(now + t.rto);
                }
                if synack {
                    t.rcv_adv = t.rcv_nxt.wrapping_add(wnd);
                    t.ack_now = false;
                }
            }
            return;
        }
        _ => {}
    }

    // Synchronized: data, then FIN.
    if !matches!(t.state, TcpState::TimeWait | TcpState::FinWait2) {
        loop {
            let data_end = t.snd_una.wrapping_add(tx.len() as u32);
            let in_flight = t.snd_nxt.wrapping_sub(t.snd_una);
            let wnd_allowed = t.snd_wnd.min(t.cwnd);
            if lt(t.snd_nxt, data_end) {
                let off = in_flight as usize;
                let unsent = data_end.wrapping_sub(t.snd_nxt) as usize;
                let mut room = wnd_allowed.saturating_sub(in_flight) as usize;
                if room == 0 && t.force_probe {
                    room = 1; // zero-window probe
                }
                t.force_probe = false;
                let n = unsent.min(room).min(t.mss as usize);
                if n == 0 {
                    if t.snd_wnd == 0 && in_flight == 0 && t.timer.is_none() {
                        t.persist = true;
                        t.timer = Some(now + t.rto);
                    }
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
                let h = Hdr {
                    seq: t.snd_nxt,
                    ack: t.rcv_nxt,
                    flags,
                    wnd: wnd16,
                    mss: false,
                };
                emit(iface, stats, now, local, remote, h, Some((tx, off, n)));
                let fresh = !lt(t.snd_nxt, t.snd_max);
                t.snd_nxt = t.snd_nxt.wrapping_add(n as u32);
                if fin {
                    sent_fin(t, data_end);
                }
                if gt(t.snd_nxt, t.snd_max) {
                    t.snd_max = t.snd_nxt;
                }
                if fresh && t.rtt_probe.is_none() {
                    t.rtt_probe = Some((t.snd_nxt, now));
                }
                if t.timer.is_none() || t.persist {
                    t.persist = false;
                    t.timer = Some(now + t.rto);
                }
                t.rcv_adv = t.rcv_nxt.wrapping_add(wnd);
                t.ack_now = false;
                if fin {
                    break;
                }
            } else if t.fin_queued && !t.fin_acked && t.snd_nxt == data_end {
                let h = Hdr {
                    seq: data_end,
                    ack: t.rcv_nxt,
                    flags: FIN | ACK,
                    wnd: wnd16,
                    mss: false,
                };
                emit(iface, stats, now, local, remote, h, None);
                t.snd_nxt = data_end.wrapping_add(1);
                sent_fin(t, data_end);
                if gt(t.snd_nxt, t.snd_max) {
                    t.snd_max = t.snd_nxt;
                }
                if t.timer.is_none() || t.persist {
                    t.persist = false;
                    t.timer = Some(now + t.rto);
                }
                t.rcv_adv = t.rcv_nxt.wrapping_add(wnd);
                t.ack_now = false;
                break;
            } else {
                break;
            }
        }
    }
    if t.ack_now {
        let h = Hdr {
            seq: t.snd_nxt,
            ack: t.rcv_nxt,
            flags: ACK,
            wnd: wnd16,
            mss: false,
        };
        emit(iface, stats, now, local, remote, h, None);
        t.rcv_adv = t.rcv_nxt.wrapping_add(wnd);
        t.ack_now = false;
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

/// Retransmit the first unacknowledged segment (fast retransmit), leaving snd_nxt alone.
fn retransmit_first(st: &mut Stack, i: usize) {
    let now = st.now;
    let Stack {
        iface,
        socks,
        stats,
        ..
    } = st;
    let Some(s) = socks.get(i) else { return };
    let (local, remote) = (s.local, s.remote);
    let wnd = rcv_wnd(&s.rx);
    let t = &mut s.tcb;
    let n = s.tx.len().min(t.mss as usize);
    let fin = n == s.tx.len() && t.fin_seq.is_some() && !t.fin_acked;
    if n == 0 && !fin {
        return;
    }
    let h = Hdr {
        seq: t.snd_una,
        ack: t.rcv_nxt,
        flags: ACK | if fin { FIN } else { 0 },
        wnd: wnd as u16,
        mss: false,
    };
    emit(iface, stats, now, local, remote, h, Some((&s.tx, 0, n)));
    stats.tcp_retrans_segs += 1;
    t.retransmits += 1;
    t.rtt_probe = None;
    t.timer = Some(now + t.rto);
}

// ---- segment input --------------------------------------------------------------------------

struct Seg<'a> {
    sport: u16,
    dport: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    wnd: u32,
    mss: Option<u16>,
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
    let mut mss = None;
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
                if kind == 2 && len == 4 {
                    mss = Some(be16(p, k + 2));
                }
                k += len;
            }
        }
    }
    Some(Seg {
        sport: be16(p, 0),
        dport: be16(p, 2),
        seq: be32(p, 4),
        ack: be32(p, 8),
        flags: p[13],
        wnd: be16(p, 14) as u32,
        mss,
        data: &p[hl..],
    })
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
        if s.local.port != local.port {
            continue;
        }
        if s.local.ip == local.ip {
            return Some(i);
        }
        if s.local.ip.is_unspecified() {
            wild = Some(i);
        }
    }
    wild
}

/// A TCP segment arrived (IP payload, addresses from the IP header).
pub fn input(st: &mut Stack, src: Ipv4, dst: Ipv4, p: &[u8]) {
    st.stats.tcp_in_segs += 1;
    if dst.is_broadcast() || dst.is_multicast() || src.is_broadcast() {
        return;
    }
    if finish(sum(p, pseudo_sum(src, dst, PROTO_TCP, p.len()))) != 0 {
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
    } else if let Some(l) = find_listener(st, local) {
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
    let s = st.socks.get(c).unwrap();
    s.user_open = false;
    s.local = local;
    s.remote = remote;
    let t = &mut s.tcb;
    t.start(
        initial_seq(local, remote, now),
        seg.mss.unwrap_or(DEFAULT_MSS).min(OUR_MSS),
    );
    t.state = TcpState::SynReceived;
    t.irs = seg.seq;
    t.rcv_nxt = seg.seq.wrapping_add(1);
    t.snd_wnd = seg.wnd;
    t.snd_wl1 = seg.seq;
    t.snd_wl2 = 0;
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
    t.mss = seg.mss.unwrap_or(DEFAULT_MSS).min(OUR_MSS);
    t.cwnd = 10 * t.mss as u32;
    if has_ack {
        t.snd_una = seg.ack;
        t.snd_wnd = seg.wnd;
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
    let wnd = rcv_wnd(&s.rx);
    let t = &mut s.tcb;

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
                if state == TcpState::TimeWait && seg.flags & FIN != 0 {
                    t.deadline = Some(now + 2 * MSL);
                }
                output(st, i);
            }
            return;
        }
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
            t.snd_wnd = seg.wnd;
            t.snd_wl1 = seg.seq;
            t.snd_wl2 = seg.ack;
            t.timer = None;
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
    let mut woke = false;
    let mut fast_rexmit = false;
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
        if let Some((pseq, at)) = t.rtt_probe
            && ge(seg.ack, pseq)
        {
            t.rtt_probe = None;
            t.sample_rtt(now.saturating_sub(at));
        }
        t.retries = 0;
        // New data got through: drop the timeout backoff (as BSD does). Without it, Karn's
        // rule (no samples from resent data) can leave the RTO doubled for good under loss.
        t.rto = t.rto_base;
        let mss = t.mss as u32;
        if t.dupacks >= 3 {
            t.cwnd = t.ssthresh; // leave fast recovery
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
        && seg.wnd == t.snd_wnd
        && t.snd_una != t.snd_max
    {
        t.dupacks = t.dupacks.saturating_add(1);
        if t.dupacks == 3 {
            let flight = t.snd_max.wrapping_sub(t.snd_una);
            let mss = t.mss as u32;
            t.ssthresh = (flight / 2).max(2 * mss);
            t.cwnd = t.ssthresh + 3 * mss;
            fast_rexmit = true;
        }
    }
    // Window update (RFC 9293: only from a segment newer than the last update).
    if lt(t.snd_wl1, seg.seq) || (t.snd_wl1 == seg.seq && le(t.snd_wl2, seg.ack)) {
        if seg.wnd > t.snd_wnd {
            woke = true;
        }
        t.snd_wnd = seg.wnd;
        t.snd_wl1 = seg.seq;
        t.snd_wl2 = seg.ack;
    }
    if t.snd_wnd == 0 && seg.ack == t.snd_una {
        t.retries = 0; // the peer is alive, just full
    }
    if woke {
        st.wake(i);
    }
    if fast_rexmit {
        retransmit_first(st, i);
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
                    let h = Hdr {
                        seq: t.snd_nxt,
                        ack: 0,
                        flags: RST,
                        wnd: 0,
                        mss: false,
                    };
                    let (l, r) = (s.local, s.remote);
                    emit(&mut st.iface, &mut st.stats, now, l, r, h, None);
                    finished(st, i);
                    return;
                }
                if seq == t.rcv_nxt {
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
                    st.wake(i);
                } else {
                    // Out of order: keep it in the ring past the readable end (if there is
                    // room in the gap list), and ACK rcv_nxt so the peer fills the hole.
                    fin = false;
                    if !s.shut_rd {
                        let off = seq.wrapping_sub(t.rcv_nxt) as usize;
                        let n = s.rx.write_at(off, data);
                        if n > 0 {
                            ooo_insert(t, seq, seq.wrapping_add(n as u32));
                        }
                    }
                }
                let s = st.socks.get(i).unwrap();
                s.tcb.ack_now = true;
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
    t.deadline = Some(now + 2 * MSL);
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
    s.error = Some(e);
    s.tx.clear();
    finished(st, i);
}

fn send_rst_now(st: &mut Stack, i: usize) {
    let now = st.now;
    let Some(s) = st.socks.get(i) else { return };
    let h = Hdr {
        seq: s.tcb.snd_nxt,
        ack: s.tcb.rcv_nxt,
        flags: RST | ACK,
        wnd: 0,
        mss: false,
    };
    let (l, r) = (s.local, s.remote);
    emit(&mut st.iface, &mut st.stats, now, l, r, h, None);
}

// ---- timers -------------------------------------------------------------------------------------

/// Run TCP timers (from the 100 Hz tick).
pub fn tick(st: &mut Stack) {
    let now = st.now;
    for i in 0..super::socket::MAX_SOCKETS {
        let Some(s) = st.socks.get(i) else { continue };
        if s.ty != SockType::Stream {
            continue;
        }
        let t = &s.tcb;
        if let Some(d) = t.deadline
            && now >= d
        {
            match t.state {
                TcpState::TimeWait => {
                    finished(st, i);
                    continue;
                }
                TcpState::FinWait2 if !s.user_open => {
                    finished(st, i);
                    continue;
                }
                _ => {}
            }
        }
        if let Some(d) = t.timer
            && now >= d
        {
            on_timer(st, i);
        }
    }
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
    st.stats.tcp_retrans_segs += 1;
    let s = st.socks.get(i).unwrap();
    let t = &mut s.tcb;
    t.retransmits += 1;
    t.rto = (t.rto * 2).min(RTO_MAX);
    t.rtt_probe = None;
    if handshake {
        t.snd_nxt = t.iss;
    } else {
        let flight = t.snd_max.wrapping_sub(t.snd_una);
        let mss = t.mss as u32;
        t.ssthresh = (flight / 2).max(2 * mss);
        t.cwnd = mss;
        t.dupacks = 0;
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
    let iss = initial_seq(s.local, remote, now);
    let t = &mut s.tcb;
    t.start(iss, OUR_MSS);
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
    let s = st.socks.get(i).unwrap();
    if buf.is_empty() {
        return Ok(0);
    }
    if !s.rx.is_empty() {
        let n = s.rx.pop(buf);
        // Window update once the window has opened by a useful amount.
        let t = &mut s.tcb;
        if t.state.synchronized() {
            let edge = t.rcv_nxt.wrapping_add(rcv_wnd(&s.rx));
            let grow = edge.wrapping_sub(t.rcv_adv);
            if (grow as i32) > 0
                && grow as usize >= (t.mss as usize).min(super::socket::BUF_SIZE / 2)
            {
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
