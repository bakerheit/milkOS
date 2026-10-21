# Networking

Files: `kernel/src/virtio/net.rs` (driver), `kernel/src/net/` (stack, socket API, /proc/net,
demo server, self-test), `kernel/src/proc/sock.rs` (socket syscalls), `user/rt/src/net.rs` +
`user/fetch` (user side), `scripts/net-test.sh` + helpers (end-to-end test).

Everything is written from scratch (no crates). IPv4 only: up to two Ethernet interfaces
(`eth0`, the slirp NIC with DHCP; `eth1`, an optional VM-to-VM link with a link-local
address, see "eth1" below) plus loopback (`lo`).

## Running it

| | |
|---|---|
| `scripts/run.sh` / `screenshot.sh` | `-netdev user,id=n0,restrict=on` + `virtio-net-device` (`scripts/netdev.sh`). The guest gets 10.0.2.15 by DHCP and can ping 10.0.2.2; slirp lets it reach nothing else (no host, no internet; it also hands out no router/DNS in this mode). |
| `MILK_HTTP_PORT=8088 scripts/run.sh` | also forwards `127.0.0.1:8088` on the Mac to the guest's port 80: `curl http://127.0.0.1:8088/`. Opt-in so parallel qemus don't fight over the port. |
| `scripts/net-test.sh` | the full end-to-end test (below). Unrestricted slirp, but every host-side helper binds to 127.0.0.1 and the guest is only told about 10.0.2.2 (= the Mac's loopback). |
| in msh | `ifconfig`, `netstat [-t] [-u] [-l] [-s]`, `ping [-c N] [-i MS] 10.0.2.2`, `fetch http://127.0.0.1/`, `nc HOST PORT` / `nc -l PORT`, `cat /proc/net/tcp`. Host servers at 10.0.2.2:PORT (the Mac's 127.0.0.1) are only reachable without `restrict=on`. |

Network safety rule for tests: only qemu's own gateway/DHCP (10.0.2.2) and servers we start
on the Mac at 127.0.0.1 (seen from the guest as 10.0.2.2:PORT). No internet hosts, no real
DNS (see the resolver policy below).

## Structure

```text
            socket API (net::socket)          syscalls (proc/sock.rs) -> sockfile -> socket API
                 |                                         
   tcp.rs   udp.rs   icmp.rs   dhcp.rs      dns.rs (thread context, uses the socket API)
        \      |       /        /
         Stack::ip_input / Stack::send_ip   (mod.rs)
                 |
   iface.rs: routing, ARP (arp.rs), parked packets, loopback ring
                 |
   virtio/net.rs: rx/tx virtqueues
```

* **One lock.** All state is one `Stack` behind one `SpinLock` (`net::with(|st| ...)`). It is
  held only for short non-blocking work (parse a frame, queue bytes, build segments), never
  across a sleep. Every API call is `with(...)`, which afterwards also delivers loopback
  packets and wake-ups.
* **The net thread** (`[net]`, High priority, started by `net::init`) sleeps on a `WaitQueue`
  with a timeout = the next timer. It is woken by the virtio-net IRQ (which only acks the
  interrupt status and calls `wake_all`, nothing else: no lock, no allocation), by any
  readiness change, and by API calls that arm an earlier timer. Each pass: drain rx, reclaim
  tx, run timers (TCP retransmission / persist / TIME_WAIT, ARP retries + expiry, DHCP), run
  the in-kernel services (HTTP on :80). Idle, it costs nothing (`ps` shows 0.00 s).
* **Memory.** The device's rings and buffers are allocated once at boot.
  * **Socket buffers** are per socket, from the PMM (`net::ring::Ring`): 64 KiB each way
    to start. TCP grows them to 4 MiB as needed (below); all socket buffers together are
    capped at 64 MiB.
  * Frames are built straight into device tx buffers, and received frames are parsed in
    place in their rx buffer.
  * When a device or the loopback queue is full, TCP doesn't count that as a loss: it
    stops and tries again 2 ms later.
* **Loopback**: 127/8 and our own address go into a 256-packet ring that `with` drains after
  the call (bounded), so a loopback connect/accept/transfer completes inside the calls.

## Protocols

* **virtio-net**: features MAC + STATUS only (no offloads, no mergeable buffers, no control
  queue). 12-byte `virtio_net_hdr` (VERSION_1), sent zeroed. Up to 64 rx and 64 tx
  descriptors, 2 KiB buffer each.
* **Ethernet / ARP**: per-interface 32-entry cache, requests rate-limited to 1/s, 3 tries; up to 8 packets
  wait (3 s) for an answer, then are dropped. Entries live 10 minutes. The gateway is
  resolved right after DHCP.
* **IPv4**: header checksum checked, TTL 64. DF is set on everything sent whole: TCP
  never needs fragments.
  * **Fragmenting**: a UDP datagram over 1472 bytes (up to 65507) goes out as IP fragments
    of 1480 bytes (`Stack::send_ip_fragments`). With an unresolved next hop only the first
    8 wait for ARP.
  * **Reassembly** (`net/frag.rs`) has limits:
    * at most 16 datagrams at a time and 1 MiB buffered (the oldest goes first), each
      dropped after 30 s;
    * a bitmap of 8-byte blocks records what has arrived. **Any overlap**, an exact
      duplicate included, poisons the datagram: it is dropped, and so are its later
      fragments until the timeout (RFC 5722's rule, which defeats overlap tricks);
    * it is also poisoned by: a fragment with MF that isn't a multiple of 8 bytes; more
      than 65535 bytes; two different last fragments; data past the end; or, for TCP, a
      fragment at offset 8 or a first fragment shorter than the TCP header (RFC 1858);
    * the whole packet gets the first fragment's header and goes through `ip_input` again;
    * `/proc/net/snmp` adds `ReasmReqds ReasmOKs ReasmTimeout ReasmOverlaps FragCreates`. Routing: loopback, on-link (per interface), link-local and multicast to
eth1, else the default gateway (eth0's).
* **ICMP**: echo replies; our pings (`socket::ping`); destination unreachable (port /
  protocol) for closed UDP ports and unknown protocols.
* **UDP**: checksums both ways, datagrams queued as records in the socket's rx ring,
  `connect` filters the peer.
* **DHCP**: DISCOVER/OFFER/REQUEST/ACK with retries (1, 2, 4, 8 s, then every 8 s), renews
  at T1, starts over at expiry or NAK. Boot does not wait for it beyond the self-test's 3 s.
* **DNS** (`net::dns::resolve(name, timeout_ms)`): A records, 2 tries. **Policy**: never a
  real lookup. `localhost`/`*.localhost` are 127.0.0.1 and dotted quads parse locally; any
  other name must end in `.test` (else `EPERM`) and goes only to a server set with
  `dns::set_server`, which must be loopback or on our subnet (else `EPERM`). The DHCP DNS
  (slirp's 10.0.2.3, which forwards to the Mac's real resolver) is recorded but never used.
  Relax this deliberately when the project wants real name resolution.
* **TCP** (`net/tcp.rs`):
  * **Basics.** The RFC 9293 state machine (all 11 states); active, passive and
    simultaneous open; FIN in every state.
  * **MSS.** We announce 1460 and assume 536 if the peer sends none. Data per segment is
    the MSS minus the options it carries.
  * **Options** (RFC 7323, RFC 2018), offered in our SYN and used when both sides agree:
    * **Window scaling**: we use shift 7, so windows up to 8 MiB. A SYN's window is never
      scaled.
    * **Timestamps**: a millisecond clock. Every ACK of new data gives an RTT sample from
      the echo. PAWS drops old duplicates (a TSval below TS.Recent).
    * **SACK**: up to 3 blocks with timestamps (4 without), the newest first.
  * **Retransmission timer** (RFC 6298): RTO from SRTT/RTTVAR, between 200 ms and 60 s.
    Karn's rule applies when there are no timestamps. Backoff is exponential and reset by
    new ACKs.
  * **Congestion control**: slow start and congestion avoidance (RFC 5681), initial window
    10 segments.
  * **Loss recovery.** It starts on 3 duplicate ACKs, or with SACK when more than two
    segments' worth is SACKed above the first hole (RFC 6675 IsLost).
    * cwnd = ssthresh = flight/2.
    * The holes below the highest SACKed byte are retransmitted first, the first one at
      once and then while pipe < cwnd.
    * Without SACK it is NewReno: one retransmission per partial ACK (RFC 6582).
    * It ends when the ACK passes the recovery point.
    * A timeout means slow start from `snd_una`, with the scoreboard dropped.
  * **ACKs and Nagle.**
    * Delayed ACKs: every second full segment, else after 40 ms. The ACK goes at once for
      out-of-order data (with SACK), a filled hole, a FIN, or a window that opened by
      2 MSS or half the buffer.
    * Nagle, Minshall's variant: a small segment waits only while an earlier small one is
      unacknowledged, so bulk transfers don't stall on their tail. `TCP_NODELAY` turns it
      off.
  * **Buffer auto-sizing.** The receive buffer doubles when the application reads more
    than half of it in one round trip (RTT from timestamp echoes). The send buffer doubles
    when it, not cwnd or the peer's window, is the limit. Both start at 64 KiB and stop at
    4 MiB. `SO_RCVBUF` / `SO_SNDBUF` set a size and turn auto-sizing off for that buffer
    (buffers only grow).
  * **Out-of-order segments** are kept in the receive ring past its readable end (up to 32
    ranges) and committed when the hole fills.
  * **Zero windows**: zero-window probes (persist timer).
  * **RST.**
    * RFC 5961 challenge ACKs for in-window RST/SYN.
    * RST for segments to closed ports.
    * RST instead of FIN when closing with unread data (as Linux).
  * **TIME_WAIT** lasts 2 x MSL = 60 s (as Linux), in a compact table (`tcp::TwTable`, up
    to 512 entries, the oldest dropped first).
    * An entry holds no socket slot or buffers, so short connections don't use up the 32
      slots; it is parked there as soon as the user has closed the socket.
    * In TIME_WAIT: a retransmitted FIN is re-ACKed (and the 2 MSL restarts), RSTs are
      ignored (RFC 1337), and other segments get an ACK.
    * A new SYN with a higher sequence number, or with timestamps a newer TSval, reuses the
      pair (RFC 6191). Our own `connect` to a pair we hold in TIME_WAIT reuses it too (our
      ISN is clock-based).
    * `/proc/net/tcp` lists the entries (state 06).
  * **Limits**: a SYN is retried 5 times (~31 s) and data 10 times, then `ETIMEDOUT`
    (+RST). An orphaned FIN_WAIT_2 lasts 30 s. No keepalive, no urgent data.

## IPv6 (`net/ipv6.rs`)

* **Addresses** (`/proc/net/if_inet6`, `ifconfig`):
  * every link gets a link-local `fe80::<modified EUI-64 of the MAC>/64`;
  * **eth0** gets a SLAAC address from qemu slirp's router advertisement: prefix
    `fec0::/64`, router `fe80::2`, so `fec0::5054:ff:fe12:3456`. slirp's host is
    `fec0::2`, as 10.0.2.2 is for IPv4, and it reaches the Mac's `::1`. `restrict=on`
    keeps everything else in: IPv6 goes no further than IPv4 does;
  * **eth1**, which has no router, also gets a unique-local address of its own,
    `fd6d:696c:6b00:1::<EUI-64>/64` ("milk", subnet 1; a constant global ID, which is
    fine on a private two-VM link), so the VMs reach each other without scope ids;
  * `::1` is loopback;
  * every address goes through duplicate address detection: one NS from `::` to its
    solicited-node group, then 500 ms of quiet. A conflict (an NA for it, or another
    host's DAD probe) drops the address, loudly.
* **NDP** (RFC 4861):
  * a neighbour cache per link, filled from NS/NA/RA link-layer options and our NS to the
    solicited-node group (1 s retry, 3 tries, entries live 10 min);
  * packets wait for an answer (16 at most, 3 s);
  * RS on eth0 once its link-local is up (3 tries, 4 s apart). The RA gives the default
    router, on-link prefixes, SLAAC prefixes (A flag, /64) and the MTU;
  * ND messages are only taken with hop limit 255.
* **Ethernet**: ethertype 0x86dd. Frames to `33:33:*` are taken on every link (NDP needs
  them), and multicast goes out to `33:33:<last 32 bits>`.
* **Routing and source selection**:
  * `::1` and our own addresses go to loopback;
  * a link-local or link-scope multicast destination goes out of the link given by the
    scope id, else the link whose neighbour cache knows it, else eth1, else eth0;
    `ff02::fb` goes to eth1;
  * a destination on a link's prefix goes out of that link; anything else goes to the
    default router (eth0's);
  * the source is the link-local for link-local and multicast destinations, else the
    address on the same prefix.
* **ICMPv6** (RFC 4443):
  * echo request/reply;
  * destination unreachable (port, for closed UDP ports) and parameter problem;
  * no errors about errors or about multicast destinations, at most 1232 bytes of the
    offending packet quoted.
* **Extension headers**:
  * hop-by-hop options (only as the first header) and destination options: Pad1/PadN
    are skipped, unknown options are handled by their action bits (skip, discard, or
    discard with a parameter problem);
  * **routing headers: no source routing, ever.** With segments left > 0 the packet is
    dropped with a parameter problem (RFC 5095 deprecates type 0; we route for no one).
    With 0 left the header is skipped;
  * fragment headers: an atomic fragment (offset 0, no M, RFC 6946) is skipped. Real
    fragments are reassembled (`net/frag.rs`, the same table and limits as IPv4: 16
    datagrams, 1 MiB, 30 s; any overlap poisons the datagram, RFC 5722). The first
    fragment must hold the whole header chain and 8 bytes of the upper-layer header (20
    for TCP), RFC 7112. The reassembled packet (fragment header removed) goes through
    IPv6 input again. UDP over IPv6 sends fragments above the path MTU (up to 65527
    bytes); TCP never needs them;
  * no next header (59) ends the packet.
* **Multicast and MLD** (`net/mld.rs`, MLDv2 host side, RFC 3810): a link takes IPv6
  multicast only for groups it is in: all-nodes, the solicited-node group of each of its
  addresses, and the groups sockets joined (IPV6_JOIN_GROUP, `struct ipv6_mreq`); the
  Ethernet filter checks `33:33:<group>` the same way. Joins and leaves go to `ff02::16`
  as state-change reports (twice, 1 s apart; link-local source, hop limit 1, router alert
  in a hop-by-hop header); queries get a current-state report within their maximum
  response time (at most 1 s). No source filtering. `/proc/net/igmp6` lists the groups.
* **Path MTU discovery** (`net/pmtu.rs`, both families; RFC 1191, RFC 8201): ICMP
  fragmentation needed (IPv4 TCP goes out with DF) and ICMPv6 packet too big lower the
  path MTU to that destination (floors 552 and 1280, only down, 10 minutes). For TCP only
  when the quoted segment is in flight on the connection (RFC 5927); the connection then
  takes the smaller MSS and resends at once, without a congestion response. New
  connections start under the cached path MTU, UDP fragments at it. `/proc/net/pmtu`.
* **Boot self-test**: link-local and SLAAC on eth0, the router `fe80::2`, echo to `::1`,
  to our own link-local and to `fec0::2` (slirp), crafted packets for each
  extension-header rule, reassembly (fragments last-first, an overlap, a split header
  chain), MLD membership (a group's datagram is dropped before a join and after the
  leave), and path MTU over loopback (below).

### Dual stack: TCP and UDP over IPv6

One socket layer for both families (`wire::IpAddr`, `SockAddr { ip: IpAddr, port }`):

* **AF_INET6 sockets** carry a family, `v6only` (IPV6_V6ONLY) and a scope (the link of a
  link-local peer or bind address). An AF_INET6 wildcard (`[::]`) listener is
  **dual-stack** unless IPV6_V6ONLY: it takes IPv4 connections and datagrams too, and user
  space sees their peers as `::ffff:a.b.c.d` (inside the stack an IPv4-mapped address is
  just IPv4). Port sharing follows Linux: a dual-stack `[::]:P` and `0.0.0.0:P` clash
  (`EADDRINUSE`), a v6-only one and an IPv4 one can share the port; for an IPv4 packet an
  AF_INET listener beats a dual-stack one. AF_INET sockets refuse IPv6 addresses
  (`EAFNOSUPPORT`), a v6-only socket refuses IPv4 ones (`ENETUNREACH`).
* **TCP** over IPv6 is the same TCP (options, SACK, timers, TIME_WAIT); only the
  checksum pseudo-header, the MSS (1440: 1500 - 40 - 20) and the output path differ. A
  connection accepted on a link-local address keeps that link for its replies.
* **UDP** over IPv6 requires the checksum (RFC 8200) and never fragments: 1452 bytes at
  most (`EMSGSIZE`). Received datagrams remember the sender's link, which becomes
  `sin6_scope_id` in recvfrom, so a reply to a link-local sender goes back out of the
  same link.
* **Ping sockets** over IPv6: `socket(AF_INET6, SOCK_DGRAM, IPPROTO_ICMPV6)`, echo request
  type 128 in, replies (129) for the socket's identifier back.
* Self-test (`dual-stack`): a v6 and a v4 client on one `[::]` listener over loopback
  (peers `::1` and 127.0.0.1, MSS 1440), the V6ONLY sharing and clash rules, the two
  "wrong family" errors, and a UDP datagram over `::1`.

## eth1: a second network for VM-to-VM links

A second `virtio-net-device` becomes `eth1`. Interfaces are named in qemu command-line
order (qemu gives the first `-device` the highest virtio-mmio slot; `Iface::attach` sorts
by slot, highest first), so with one NIC nothing changes and in a two-NIC boot the first
one (slirp) stays eth0 with DHCP exactly as before. eth1:

* **Addressing**: IPv4 link-local (RFC 3927, simplified): the candidate is 169.254.X.Y
  from the last two MAC bytes (`02:6d:6b:64:0a:0a` -> 169.254.10.10, bytes kept in
  1..=254), probed with two ARP probes (sender 0.0.0.0) 200 ms apart, then taken (/16, no
  gateway) and announced with a gratuitous ARP. A probe or claim for our candidate/address
  from another MAC is a conflict: logged (`[net] eth1: link-local A is taken by MAC;
  trying B`), a new candidate is mixed from the MAC and the conflict count, and probing
  starts over. Two boots whose MACs derive the same address end up on different ones.
* **Routing**: 169.254/16 and multicast go out of eth1, everything else as before (eth0
  never sends or takes multicast, as before). Each link has its own ARP cache; the parked
  packet pool is shared and remembers the link.
* **Multicast**: link-scope only (224.0.0.0/24, never routed). eth1 accepts frames to
  `01:00:5e:*` and delivers 224.0.0.x datagrams to the UDP socket bound to the port (one
  socket, as for unicast); no IGMP (the link has no router to tell). Sending to a group
  uses the RFC 1112 MAC. No ICMP errors for multicast.
* **The wire**: qemu `-netdev dgram` between two qemus over 127.0.0.1 UDP (each side's
  `local` is the other's `remote`), so it needs no start order and no root; frames,
  including broadcasts and multicast, go to the one peer. `scripts/drop-test.sh` boots such
  a pair.
* `/proc/net/dev`, `arp`, `route` (169.254/16 and 224/4 on eth1) and `ifaddr`
  (`linklocal=bound|probing conflicts=N`) list eth1; `ifconfig` shows it.

## The kernel socket API (`kernel/src/net/socket.rs`)

All calls are **non-blocking** and take/return Linux errno values (`fs::errno::Errno`,
extra codes in `net::errno`), so a syscall returns `-(e.0)` unchanged. Where Linux would
block they return `EAGAIN` (`EINPROGRESS` for connect); `poll` tells when to retry; `wait`
sleeps until then.

```rust
use crate::net::socket::{self, SockType, SocketId, Shutdown, POLLIN, POLLOUT};
use crate::net::wire::{Ipv4, SockAddr};

pub fn socket(ty: SockType) -> Result<SocketId>                 // Stream (TCP) | Dgram (UDP); ENOBUFS when 32 are in use
pub fn socket_family(f: Family, ty: SockType) -> Result<SocketId>   // Family::V6: AF_INET6 (dual-stack until set_v6only)
pub fn socket_icmp() -> Result<SocketId>                        // ping socket (Dgram, ICMP echo; see the ping row below)
pub fn socket_icmp6() -> Result<SocketId>                       // the same for ICMPv6 echo
pub fn set_v6only(s, on) / v6only(s) / family(s)                // IPV6_V6ONLY (before bind; ENOPROTOOPT on AF_INET)
pub fn set_scope(s, Option<usize>) / scope(s)                   // the link for a link-local address (sin6_scope_id)
pub fn bind(s: SocketId, addr: SockAddr) -> Result<()>          // port 0 = ephemeral (49152..); EADDRINUSE, EADDRNOTAVAIL, EINVAL (bound)
pub fn listen(s: SocketId, backlog: usize) -> Result<()>        // TCP; backlog clamped to 1..=16; autobinds
pub fn accept(s: SocketId) -> Result<(SocketId, SockAddr)>      // EAGAIN if none ready; EINVAL if not listening
pub fn connect(s: SocketId, addr: SockAddr) -> Result<()>       // TCP: EINPROGRESS, then EALREADY while in progress,
                                                                //   Ok once after it completed (then EISCONN), or the error
                                                                //   (ECONNREFUSED, ETIMEDOUT, ENETUNREACH); UDP: sets the peer
pub fn send(s: SocketId, buf: &[u8]) -> Result<usize>           // short count possible; EAGAIN (full / handshaking), EPIPE, ENOTCONN,
                                                                //   ECONNRESET once after a reset; UDP needs connect (EDESTADDRREQ)
pub fn sendto(s: SocketId, buf: &[u8], to: SockAddr) -> Result<usize>   // UDP datagram (<= 1472, EMSGSIZE); TCP ignores `to`
pub fn recv(s: SocketId, buf: &mut [u8]) -> Result<usize>       // Ok(0) = EOF; EAGAIN; ECONNRESET once; UDP: one datagram, truncated
pub fn recvfrom(s: SocketId, buf: &mut [u8]) -> Result<(usize, SockAddr)>
pub fn shutdown(s: SocketId, how: Shutdown) -> Result<()>       // Read | Write (FIN after queued data) | Both; ENOTCONN
pub fn close(s: SocketId) -> Result<()>                         // handle dead at once; TCP finishes in the background
pub fn poll(s: SocketId) -> u32                                 // POLLIN POLLOUT POLLERR POLLHUP POLLRDHUP (+RDNORM/WRNORM), POLLNVAL
pub fn take_error(s: SocketId) -> Result<Option<Errno>>         // SO_ERROR
pub fn local_addr(s) / peer_addr(s) -> Result<SockAddr>         // getsockname / getpeername (ENOTCONN)
pub fn sock_type(s) -> Result<SockType>                         // SO_TYPE
pub fn send_capacity(s) -> Result<usize>                        // free send-buffer space (UDP: 1472)
pub fn pending_input(s) -> Result<usize>                        // FIONREAD (UDP: next datagram's size)
pub fn count() -> usize                                         // sockets in use, background ones included

// Blocking helpers (thread context only, they sleep):
pub fn wait(s: SocketId, mask: u32, timeout_ns: u64) -> u32     // until poll & (mask|ERR|HUP|NVAL) != 0; returns those bits, 0 = timeout
pub fn ping(dst: Ipv4, seq: u16, timeout_ns: u64) -> Result<PingReply>   // ICMP echo; ETIMEDOUT

// Readiness notification:
net::set_wake_hook(fn(SocketId))   // called (thread context, stack lock not held) when a socket's readiness may have changed
net::ready_generation() -> u64     // bumped on every change
net::READY_WQ                      // WaitQueue woken on every change (what `wait` sleeps on)
```

Other entry points: `net::init()` (after `sched::init`), `net::selftest::run()`,
`net::config() -> IpConfig { addr, mask, gateway, dns }`, `net::mac()`, `net::poll()`,
`net::dns::{resolve, set_server, server}`.

`SocketId` is slot + generation (`raw()` / `from_raw()` to store it); a stale id gives
`EBADF`. Semantics follow Linux unless noted: binding never conflicts with connections
that are closing in the background (as if SO_REUSEADDR were always set); a failed connect
may be retried on the same socket, a socket that was connected and closed cannot connect
again (`EISCONN`).

Blocking pattern (what the syscall layer does):

```rust
loop {
    match socket::recv(id, &mut buf) {
        Err(e) if e == EAGAIN && !nonblocking => { socket::wait(id, POLLIN, FOREVER); }
        r => break r,
    }
}
```

## Syscalls (`kernel/src/proc/sock.rs`)

Sockets are files: `net::sockfile::SocketFile` is an `Inode` of type `Socket` inside an
`OpenFile`, so they sit in the fd table, survive `dup`/`fork`, and `read`/`write`/`close`
work on them (the last close closes the socket).

| nr | call | notes |
|---|---|---|
| 198 | socket | `AF_INET` or `AF_INET6` (else `EAFNOSUPPORT`); `SOCK_STREAM`/`SOCK_DGRAM` + `SOCK_NONBLOCK`/`SOCK_CLOEXEC`; protocol 0/6/17, or `SOCK_DGRAM` + 1 (`IPPROTO_ICMP`, AF_INET) / 58 (`IPPROTO_ICMPV6`, AF_INET6) for a ping socket |
| 200 | bind | `struct sockaddr_in` (16 bytes) on AF_INET, `struct sockaddr_in6` (28 bytes; `sin6_scope_id` = interface index: lo 1, eth0 2, eth1 3) on AF_INET6. Addresses written back to an AF_INET6 socket's caller are always `sockaddr_in6` (IPv4 peers as `::ffff:a.b.c.d`). A `sockaddr_in` on a dual-stack AF_INET6 socket is taken as the mapped address (lenient; Linux wants `sockaddr_in6`) |
| 201 | listen | |
| 202 / 242 | accept / accept4 | blocks unless non-blocking; accept4 flags `SOCK_NONBLOCK`/`SOCK_CLOEXEC` |
| 203 | connect | blocks until connected or failed unless non-blocking (`EINPROGRESS`) |
| 204 / 205 | getsockname / getpeername | |
| 206 / 207 | sendto / recvfrom | `MSG_DONTWAIT`; blocking send queues everything before returning |
| 208 / 209 | setsockopt / getsockopt | set: SO_RCVTIMEO (recv/recvfrom/read/accept give EAGAIN after it); SO_REUSEADDR, SO_KEEPALIVE, SO_SNDBUF, SO_RCVBUF, SO_REUSEPORT, SO_SNDTIMEO, TCP_NODELAY accepted and ignored; IPV6_V6ONLY (level 41, option 26) set and get on AF_INET6 sockets; get: SO_ERROR, SO_TYPE; else `ENOPROTOOPT` |
| 210 | shutdown | |
| 73 | ppoll | any fd: sockets report real readiness, the console is readable once a line (or ^D) is typed, other files are always ready (`Inode::poll`, default POLLIN\|POLLOUT). Sleeps on the stack's readiness queue, re-checking non-socket fds every 20 ms. The sigmask is ignored. |

Not yet: `select`/`epoll`, `sendmsg`/`recvmsg`, `socketpair`, `MSG_PEEK`, `AF_UNIX`, signals interrupting a blocking call (there are no signals yet).
`read`/`write` on a socket honour `O_NONBLOCK` as of the last socket syscall on that fd
(`OpenFile` does not pass its flags to the inode; the socket syscalls re-sync them).

## /proc/net

`dev` (lo, eth0 counters), `arp`, `route` (Linux hex format), `tcp` (Linux columns, plus a
`# STATE rexmit N rto Nms` comment per line), `udp`, `snmp` (Ip/Icmp/Tcp/Udp counters),
and milkOS's `ifaddr` (`eth0 flags=UP,BROADCAST,RUNNING mtu=1500 ether=.. inet=..
netmask=.. broadcast=.. inet6=ADDR/LEN,SCOPE[,tentative] (one per IPv6 address)
router6=.. gateway=.. dns=.. dhcp=Bound`).

Also milkOS's `hostname` (the running hostname), `pmtu` (learned path MTUs) and
`igmp6` (the IPv6 groups each link is in, Linux columns, plus MLD counters).

IPv6: `tcp6` and `udp6` (AF_INET6 sockets, the 32-digit Linux address form; `tcp`/`udp`
list only AF_INET ones, so a dual-stack listener is in `tcp6`), `if_inet6` (Linux
format: address, ifindex, prefix length, scope, flags, name), `snmp6` (`Name value`
lines: Ip6, Icmp6, ND and DAD counters) and milkOS's `ndisc` (the neighbour cache).

## Userland tools (`user/`)

| program | what | how |
|---|---|---|
| `ifconfig [IFACE]` | addresses (IPv6 as `inet6 ADDR  prefixlen N  scopeid 0x20<link>`), MAC, IPv6 router, gateway/DNS/DHCP state, counters | `/proc/net/ifaddr` (milkOS, `key=value` per interface) + `/proc/net/dev` |
| `netstat [-t] [-u] [-l] [-s] [-4\|-6]` | sockets (tcp, tcp6, udp, udp6), or protocol counters | `/proc/net/{tcp,tcp6,udp,udp6,snmp,snmp6}` |
| `ping [-4\|-6] [-c N] [-i MS] [-s SIZE] [-W MS] HOST` | ICMP / ICMPv6 echo, Linux-style output | a **ping socket**: `socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP)` as on Linux (unprivileged), or `(AF_INET6, SOCK_DGRAM, IPPROTO_ICMPV6)` with `-6` or an IPv6 address. `sendto` an echo request (the kernel sets the identifier = the socket's local "port", and the checksum), `recvfrom` gets the replies for that identifier (ICMP header + data, no IP header, so no TTL), `SO_RCVTIMEO` for the timeout. Default 4 pings (no ^C yet). |
| `nc [-4\|-6] [-u] HOST PORT`, `nc -l [-4\|-6] [-u] PORT` | stdin ↔ socket | `ppoll` on fd 0 and the socket; ^D half-closes (TCP). Connect tries every address of HOST; `-l` listens on `[::]` (both families; `-4` IPv4 only, `-6` IPv6 only) |
| `httpd [-p PORT] [-q] [DIR]` | static files over HTTP/1.0 (GET, HEAD), index.html or a listing for directories, `..` refused (403), 301 for a directory without `/` | started by init as `httpd -q /srv/www` on :80 (rootfs/srv/www), listening on `[::]:80` (IPv6 and IPv4) |
| `/status` (httpd) | a live dashboard: uptime, load, CPU per core, memory, network totals, the 15 newest audit events | generated per request from /proc, in the HTML subset the desktop Browser renders (no tables or CSS: bars are text in `<pre>`); `open http://127.0.0.1/status` |
| `fetch [-i] URL` | HTTP GET | |
| `drop id / name / list / send / recv / peers / forget / visible / daemon` | Milk Drop: file sharing with another milkOS on eth1 | see "Milk Drop" below |
| `netctl [show] [-k]`, `netctl hostname / name / visible` | the network settings: interfaces, IPv4/IPv6, DHCP, DNS, hostname, the `.local` name, Milk Drop visibility; `-k` is the stable `key=value` contract for the Settings > Network panel | docs/network-panel.md; hostname via `sethostname(2)` (root, kept in `/etc/milk/net/hostname`, applied by the kernel at boot), name and visibility through the drop daemon (administrators) |

**Names in user programs** (getaddrinfo-style, both families: `rt::net::lookup_all(spec,
default_port, Want::{Any,V4,V6})` gives every address in the order to try, IPv6 first as
Linux orders them; `rt::net::connect_any(&addrs, ty)` connects to the first that answers;
`lookup` / `resolve` give the first). `SPEC` is `HOST[:PORT]`, `[V6ADDR]:PORT` or a bare
IPv6 address; `fe80::1%eth1` names the link. `ping`, `fetch`, `nc`, `tcpbench` and
`drop send` use it. One address type for both families: `rt::net::SockAddr { ip: Ip,
port, scope }`, `Ip::{V4, V6}`, with RFC 5952 text.

* a numeric address (dotted quad or IPv6): no lookup;
* `localhost`: `::1` and 127.0.0.1 (`ping localhost` stays IPv4, `ping -6 localhost` is `::1`);
* **`NAME.local`**: mDNS (RFC 6762) on eth1, `rt::mdns`. It uses the real DNS wire format
  on UDP 5353, over IPv6 to `ff02::fb` (asking AAAA and A) and over IPv4 to 224.0.0.251
  (asking A and AAAA), each a "legacy unicast" query (an ephemeral source port, one
  retry, 1.5 s); the records come straight back, and once one answer is in the other
  family gets 100 ms more. `-4`/`-6` (`Want::V4`/`V6`) ask over that family only, for
  that record type only. The responder is the Milk Drop daemon (`drop daemon`, already on
  eth1 for discovery), on `169.254.x.y:5353` and `[fd6d:696c:6b00:1::…]:5353` (which also
  takes the group `ff02::fb`). It answers A, AAAA and ANY queries for `<its drop
  name>.local` with eth1's IPv4 address and its VM-link IPv6 address (the ULA, so no scope
  id is needed; the link-local isn't announced): straight back to a legacy unicast asker
  (a link-local IPv6 asker on its own link, via the datagram's scope), else to the group
  of the family it asked on, with the cache-flush bit. The `.local` name is the
  Milk Drop name (`drop name bob` -> `bob.local`), since every machine's hostname is `milk`.
  There is no probing for name conflicts: two machines with one name both answer, and
  the first answer wins.
* **anything else is refused** (`ping: example.com: not resolved: milkOS resolves only
  numeric addresses, localhost and NAME.local (mDNS on eth1); internet names are
  refused`). The kernel's DNS policy for its own callers (`.test` only, see DNS above) is
  unchanged. Multicast never leaves through eth0, so `.local` lookups can't reach slirp or
  the Mac.

## Milk Drop (`user/drop`)

AirDrop-style sharing between two milkOS machines on the eth1 link. Everything is
userland except the eth1 link itself and one tiny fs hook (`/etc/milk/drop` is persistent).

**Identity.** Each machine has an Ed25519 key pair (`rt::ed25519`) in `/etc/milk/drop/`
(`identity.secret`, the seed, 0600 root; `identity.pub`; `name`; `known_peers`, root's
trust list), made the first time root's `drop` runs. That is normally the `drop` service
(milkd, as root) at boot. The identity is the machine's: other users' `drop` reads only the
public half and has the daemon sign over `/run/drop.sock`, and each user keeps their own
trust list in `~/.config/milk/drop/known_peers`. The model is in docs/security.md, "Milk
Drop: one machine identity, per-user clients". On a machine with a
Milk volume, `fs::init` binds `/Volumes/Milk/etc/milk/drop` onto it
(`milkfs::setup_persistent_etc`), so the identity and the trusted peers survive reboots.
The fingerprint people compare is the first 8 bytes of SHA-256(public key) in four groups
(`3fa2 91bc 07de 44a1`). The default name is `<hostname>-<first 4 fingerprint digits>`;
`drop name alice` changes it. The secret is only as safe as root: it is 0600 root, so
ordinary users' programs can't read it (root's can). `milk run-try` sandboxes keep tried apps away from
their clone's `/etc`. Keys come from the kernel CSPRNG (`/dev/urandom`: ChaCha20 seeded
from virtio-rng, docs/crypto.md), and `identity.secret` records that (`rng=kernel-csprng`).

**Old keys must be rotated.** Identities made before the kernel CSPRNG came from a weak
generator: a xorshift seeded from the boot counter, stirred with clock jitter. Such a
secret may be guessable. Their `identity.secret` has no `rng=` tag. The daemon rotates
such a key at boot by itself (`drop: carol's identity came from the old weak RNG:
rotated it ...`, a `user.drop.rotate ... reason=legacy-rng` record), and `drop id` warns
if one is still there. `drop id --rotate` does the same by hand, any time:

* a new seed from the CSPRNG;
* the old key signs `milkdrop-rotate-v1 <old> <new>`, kept in `/etc/milk/drop/rotation`
  and sent as `rot=<old>:<sig>` in announcements and hellos;
* the old secret is overwritten.

A peer that trusted the old key checks that signature and moves to the new key without
the "KEY CHANGED" refusal (`drop: alice rotated its key (signed by its old key ...)`, a
`user.drop.key_rotated` record); `drop list` shows `rotated`. Only one step is kept: a peer
that missed a rotation must `drop forget` the name. A key change *without* a valid
rotation statement is still refused.

**Discovery** ("mDNS-lite"): UDP to the mDNS group 224.0.0.251, but on port 5359 and
with a one-line text format instead of DNS records. Only milkOS machines are on the link.
`drop daemon` (started by init when eth1 exists, bound to eth1's address) multicasts every
10 s, and answers every `QUERY` at once (unicast), with:

```text
MILKDROP/1 ANNOUNCE name=alice host=milk port=5360 [addr6=<eth1 IPv6>] key=<64 hex> fp=<16 hex> recv=0|1 sig=<128 hex>
```

`addr6` is eth1's address on the VM link's IPv6 prefix, signed with the rest; a peer
takes it only if it is on `fd6d:696c:6b00:1::/64`, so an announcement can't point
transfers anywhere else.

Discovery runs over both families: the same announcements and QUERYs also go to
`[ff02::fb]:5359` from eth1's IPv6 address (the daemon joins `ff02::fb` on eth1 first;
MLD reports it). The IPv4 announcement stays for older peers. A peer heard both ways is
one entry: its IPv4 address stays `addr` (what `drop send NAME` uses) and the IPv6 one is
`addr6`; a peer heard only over IPv6 (an IPv6-only machine) has an IPv6 `addr`. The
daemon, `drop recv` and `drop id` work with either family alone.

`sig` is Ed25519 over `milkdrop-announce-v1 <the line before " sig=">` with the key it
carries, and `fp` must match that key, so an announcement can't be altered in flight or
claim someone else's key. It can be replayed, though, and trust is not decided here.
`recv=1` while `drop recv` runs. The daemon re-reads the name and the receiving flag for
every message and never makes a key. `drop list` multicasts a QUERY (and once more halfway
through), collects answers for 1.5 s, and prints name, address, fingerprint, receiving and
trust (`new` / `known` / `KEY CHANGED`). With `MILK_TABLE=1` it writes a structured table
(`drop list | where receiving == yes`). The daemon also keeps what it hears in
`/tmp/drop/peers` (`name ip fp recv seen`).

**Transfer** (TCP 5360, on eth1's address only, while `drop recv` runs): protocol 2,
**encrypted**. The hello lines end in `\n`. Everything after them is ChaCha20-Poly1305
records (`[*]`):

```text
R: MILKDROP/2 HELLO recv name=bob key=<Kr> nonce=<Nr> dh=<Er> [rot=<old>:<sig>]
S: MILKDROP/2 HELLO send name=alice key=<Ks> nonce=<Ns> dh=<Es> sig=Sign_s(T("send")) [rot=...]
R: AUTH sig=Sign_r(T("recv"))                                      (or DECLINE key-changed)
      T(role) = "milkdrop-hello-v2 role Nr Ns Kr Ks Er Es"
S: [*] OFFER name=<%-escaped> size=<n> sha256=<hex> sig=Sign_s("milkdrop-offer-v1 Nr Ns name size sha")
R: [*] ACCEPT                                                      (or DECLINE <why>)
S: [*] <the file, one record per 16 KiB, progress every 10%>
R: [*] DONE sha256=<hex>                                           (or BAD sha256=<hex>)
```

* **Key agreement.** Each side makes a fresh X25519 key pair per connection
  (`rt::crypto::x25519`, from the CSPRNG): `Er` and `Es`. Both sides sign the same
  transcript with their role. It covers both 128-bit nonces, both Ed25519 identity keys
  *and both X25519 keys*. So the existing identities authenticate the key agreement:
  someone in the middle can't substitute their own X25519 key without breaking a
  signature, and a hello can't be replayed into another session or toward another peer.
  The sender also checks that the receiver's key is the one it announced. An all-zero
  shared secret (a low-order point) is refused.
* **Records**: `[length: u32 BE][ciphertext][Poly1305 tag: 16]`, with the length as
  additional data.
  * Keys: one ChaCha20-Poly1305 key per direction,
    `SHA-256("milkdrop-v2 key s->r" | "... r->s" || shared || Nr || Ns || Er || Es)`.
  * Nonces: 4 zero bytes and a 64-bit counter per direction, so a record that is
    altered, dropped, replayed or reordered fails its tag and ends the connection.
  * Records hold at most 32 KiB.
  * The ephemeral secrets and the derived keys are wiped once used.
* Forward secrecy: the session keys come from ephemeral X25519 keys, so a stolen identity
  secret doesn't decrypt recorded transfers. It would let the thief impersonate the
  machine from then on, though (rotate!).
* Protocol 1 (plaintext) is gone: a `MILKDROP/1 HELLO` peer gets "doesn't speak Milk Drop
  2". Discovery datagrams stay `MILKDROP/1` (public, signed).
* **Trust on first use**, keyed by name and key (`known_peers`). A new key is shown with
  its fingerprint: the receiver's prompt says `NEW peer, fingerprint ...; accepting trusts
  this key`, and the sender prints `bob is new: fingerprint ... (trusted on first use)`.
  A known key under a new name is the same peer, renamed. A known name with a different key
  is refused, loudly, on either side: a boxed `WARNING: bob's KEY HAS CHANGED` with both
  fingerprints, plus an audit record. `drop forget bob` is the way out after a real
  reinstall.
* The offer is signed, so the name, size and SHA-256 are the sender's. The receiver
  streams into `~/Downloads/.<name>.part`, hashes as it goes, and renames to the final
  name only if the SHA-256 matches. The name is reduced to a safe basename, with
  ` (1)`, ` (2)`... added if it exists. A mismatch deletes the file and tells the sender.
* `drop recv` takes one transfer (received, declined or failed) and exits; `-k` keeps
  going, `-y` accepts without asking. Ctrl-C stops it (blocking socket calls now return
  `EINTR` on a pending signal).
* **Audit** (through `/dev/audit`, so recorded as `user.drop.*`):
  * `identity`, `rename`;
  * `trust` (first use);
  * `send` / `recv`: peer, fingerprint, address, file, size, sha256,
    `enc=x25519-chacha20poly1305`, `result=ok|bad|declined|refused|failed`;
  * `decline`, `key_changed`, `rotate`, `key_rotated`, `forget`.

**Hook for the desktop** (a share sheet, or Files "Send to..."): the wm doesn't need to
speak the protocol. It can run the CLI:

* peers: `MILK_TABLE=1 drop list` (a `\x1emilk-table 1` table: name, address,
  fingerprint, receiving, trust, host; see docs/shell.md), or read `/tmp/drop/peers`
  without waiting;
* send: `drop send [-4|-6] <name> <path>`. A drop name or fingerprint goes over IPv4
  (the address the announcement came from) unless `-6` (its `addr6`); `NAME.local` tries
  its addresses IPv6 first, as getaddrinfo orders them, falling back to the next one if
  refused; a numeric address (`[fd6d:…]:5360` too) is used as it is. `drop recv` listens
  on both eth1 addresses (the IPv4 one and the IPv6 one, v6-only), never on eth0. It prints `drop: [####......]  40%  416 KiB / 1024 KiB`
  progress lines, then `drop: sent ...` (exit 0) or `drop: <reason>` (exit 1). A `KEY
  CHANGED` refusal is the boxed warning on stderr;
* receive: `drop recv` prints `drop: <who> wants to send you <file> (<size>).` then
  `drop: accept? [y/N]` and reads a line from stdin, so a GUI can show its own sheet with
  the name and fingerprint and answer `y` or `n`. Finished files are in `~/Downloads`.

Kept stable for that: the line prefixes above, the table columns, the exit status.

The desktop does exactly this (`kernel/src/wm/apps/milkdrop.rs`, on a tty of its own,
`wm::tty::Job`): Files > Share... (or the toolbar's Share, or a file dropped on a NEARBY
device in the sidebar, which lists `/tmp/drop/peers`) opens the sheet, which rescans with
`MILK_TABLE=1 drop list` and runs `drop send`; the Milk Drop window (File > Receive with
Milk Drop, or `open milkdrop`) runs `drop recv -k` and answers its prompt. To see it with
two machines: `scripts/wm-drop-demo.sh ALICE_DEMO BOB_DEMO OUTDIR` (e.g. `drop-alice-send
drop-bob-wait`), two desktops with linked eth1s on private images.

**Test**: `scripts/drop-test.sh` (below).

## Throughput (`scripts/net-bench.sh`, `/bin/tcpbench`)

`tcpbench send HOST:PORT MIB | recv HOST:PORT | sink PORT | source PORT MIB` times one TCP
transfer (`send` waits for the peer's close, so the time covers delivery).
`scripts/net-bench.sh [MIB]` boots two VMs, alice and bob, and runs three paths:

* guest -> host: to a sink on the Mac's 127.0.0.1, through slirp;
* host -> guest: from a source there;
* guest -> guest: alice -> bob over eth1 (`-netdev dgram`).

The host helper is `scripts/net_bench_host.py`, bound to 127.0.0.1.

| | guest -> host | host -> guest | guest -> guest | notes |
|---|---|---|---|---|
| baseline (16 KiB windows, no scaling/SACK/timestamps, 2 s TIME_WAIT) | 98-105 MiB/s | 91-94 MiB/s | 34-37 MiB/s | 64 MiB per run, debug kernel, smp 4, three runs |
| window scaling + timestamps + SACK, delayed ACKs, Nagle, auto-sized buffers (64 KiB -> 4 MiB) | 190-210 MiB/s | 366-395 MiB/s | 59-103 MiB/s | same; guest -> guest varies with drops on the dgram link (host UDP buffers), which SACK recovery repairs |
| the same code, another machine/load (the lead's run on main) | 201 MiB/s | 293 MiB/s | 33.7 MiB/s | |

**Variance, honestly:** the slirp paths repeat within about 10%. **guest -> guest is
noisy**: 34-103 MiB/s across runs of the same build, and one run on main matched the
old baseline. The eth1 link is `-netdev dgram`: two qemu processes passing every frame
through UDP sockets on the Mac. Its throughput depends on host scheduling and on those
sockets' buffers, which overflow under large-window bursts. Those are real packet drops,
which SACK recovery has to repair. Treat the guest -> guest column as a range, not a
number; it doesn't show the TCP change on its own.

Lossy loopback (boot self-test, every Nth packet dropped, data and ACKs alike):

| | before | after |
|---|---|---|
| 1/7 | 64 KiB in 200-400 ms, go-back-N only | 256 KiB in ~2.2 s, 85 of 234 resends by SACK recovery |
| 1/3 | (not tested) | 64 KiB in ~4.2 s, intact |

## In-kernel HTTP server (demo, off by default)

`net/services.rs`, on the net thread, using only the socket API: `/` (HTML with the
interface config), `/net` (all of /proc/net), `/big` (1 MiB of numbered 32-byte lines), 404
otherwise. HTTP/1.0, `Connection: close`, 8 connections at a time. Off unless
`net::services::enable(port)` is called; the self-test does that with `khttpd=PORT` in its
fw_cfg config (net-test.sh uses 8080, for the `/big` window/retransmission checks).

## Tests

**Boot self-test** (`net::selftest`, after the sched self-test, prints `[net] selftest ok`).
It covers:

* DHCP (3 s timeout) and 3 pings to the gateway;
* UDP over loopback;
* 256 KiB + 64 KiB TCP over loopback with an orderly close both ways;
* **lossy loopback**: 256 KiB with every 7th packet dropped and 64 KiB with every 3rd
  (data, ACKs, SYN and FIN alike). The bytes must arrive intact, with window scaling,
  SACK and timestamps negotiated and SACK recovery doing some of the resending;
* **TIME_WAIT**: the closing side parks the pair without a socket slot, and a new
  connection on the same 4-tuple reuses it;
* **Path MTU** over loopback, which plays a smaller-MTU path (`iface::LO_MTU`) and answers
  too-big packets as a router would: IPv6 at 1280, a TCP connection drops to MSS 1220
  mid-stream and its 64 KiB arrive intact, then a 4000-byte UDP datagram goes out in
  1280-byte fragments and comes back whole; IPv4 at 1000 the same (MSS 960);
* **MLD**: a UDP datagram to `ff02::1:3` on eth0 is dropped until a socket joins it,
  delivered while joined, dropped after the leave, with MLDv2 reports for both;
* **IP fragments**:
  * a 20000-byte UDP datagram over loopback, as 14 fragments, back whole;
  * crafted fragments: last-first (reassembled), an overlapping pair (dropped), a TCP
    fragment at offset 8 (refused), and a lone first fragment (times out);
* **IPv6**: link-local and SLAAC on eth0, echo to `::1`, our link-local and `fec0::2`
  (slirp), the extension-header rules on crafted packets;
* **dual stack** over loopback: v6 and v4 clients on one `[::]` listener, the V6ONLY rules,
  UDP over `::1`;
* connection refused (RST), an HTTP GET to our own server, and the DNS policy. With the fw_cfg file
`opt/milkos/nettest` (`echo=IP:PORT echo6=[IPV6]:PORT dns=IP:PORT name=NAME expect=IP
refused=IP:PORT`, passed by net-test.sh) also: 512 KiB TCP echo against the Mac over IPv4
(10.0.2.2) and over IPv6 (`[fec0::2]`, which slirp maps to the Mac's `::1`), a `.test`
lookup + NXDOMAIN against the Mac's test DNS server, a refused host port. It then prints
/proc/net.

**`scripts/net-test.sh`**: starts `scripts/net_test_servers.py` (TCP echo on 127.0.0.1
and on `::1` (v6-only), test DNS and HTTP on 127.0.0.1, from a mktemp dir), boots with
hostfwds for guest :80 (userland httpd), :8080 (kernel httpd) and :7777 (nc -l), checks
the self-test, types `ifconfig` (inet6 lines checked), `ping`, `ping -6 fec0::2`, four
`fetch`es (one to `http://[::1]/`, the userland httpd over IPv6), `nc` to the host echo
server and `nc -6 fec0::2` to the `::1` one (then ^D), `nc -l 7777` (the Mac connects and
sends a line), `netstat` (httpd's `[::]:80` in tcp6) and `netstat -6 -l` into msh over the serial
console (`scripts/serial_drive.py`), then from the Mac: the userland httpd (`/`, a file, a
listing, 404, 403 for `..`, 301) and the kernel httpd (`/big` content verified, 4 parallel
`/big`, `/net`, 404). Kills only the qemu and
python processes it started, by PID.

**`scripts/drop-test.sh`**: Milk Drop between two VMs, alice and bob. Each has its own
private milkfs and scratch images in a mktemp dir, with a different random 1 MiB file put
in by `mkmilkfs.py`. eth0 is a restricted slirp; eth1 is `-netdev dgram` between two free
UDP ports on 127.0.0.1, with MACs 02:6d:6b:64:0a:0a / :0b:0b (so 169.254.10.10 /
169.254.11.11). Over the serial consoles, kept in step with marker files, it checks:

* both link-local addresses and eth0's DHCP; identities made at first boot; init's daemon;
* alice `drop list`s bob (new, receiving) and sends him her file. bob's prompt shows alice
  as NEW with her fingerprint, he types `y`, progress reaches 100%, and he verifies the
  SHA-256, which must equal the host's;
* bob sends his file to alice (now known both ways);
* bob loses his key and makes a new one under the same name. alice refuses to send to him,
  loudly, with a `user.drop.key_changed` record;
* alice `drop id --rotate`s: bob's `drop list` shows her as `rotated`, and he sends her a
  file over her new key, accepted because her old key signed it (`user.drop.key_rotated`).
  Before that, alice `drop forget`s bob's unsigned new key;
* `drop selftest` (the crypto vectors in userland), `rng: kernel CSPRNG` on new keys, and
  the kernel's `[crypto]` / `[rng]` boot lines;
* a third boot, carol, on an image whose `/etc/milk/drop` holds an old-format secret
  (a fixed seed with no `rng=` tag): her daemon rotates it at boot. On disk afterwards
  the old seed is gone, the new one is tagged, and the rotation statement is kept;
* **the wire**: qemu `-object filter-dump` writes each VM's eth1 traffic to a pcap. The
  plaintext hellos and discovery datagrams must be in it (so the capture works). The canary
  text (half of `to-bob.bin` is a known sentence repeated), the file name and the offer
  must not be;
* Ctrl-C stops a `drop recv -k` waiting in accept;
* the `user.drop.*` records on both machines;
* as milk, the desktop's account: alice-as-milk sends bob-as-milk a file, signed by each
  machine's daemon; it lands in `~/Downloads` owned by milk (root's earlier receive into
  that folder is milk's too); milk can't read `identity.secret`; milk (an administrator)
  renames the machine through the daemon (`user.drop.rename ... by uid=501`), and nobody
  is refused (`user.drop.refused`);
* after shutdown, the host reads both received files out of the images (`mkmilkfs.py
  cat`) and compares their SHA-256 with the originals, and finds alice's `known_peers` on
  her disk;
* a second boot of alice's image: same name, same fingerprint, bob still trusted;
* **IPv6 between the VMs over eth1**: alice `ping -6 bob.local` (AAAA over `ff02::fb`,
  then ICMPv6 to bob's `fd6d:696c:6b00:1:6d:6bff:fe64:b0b`) and `drop send bob.local`,
  which goes over IPv6 (IPv6 first): bob's receiver listens on both eth1 addresses, gets
  the file intact, and both audit records carry the IPv6 addresses.

It kills only its own qemus and serial drivers, by PID.

## Next

* `epoll`; a real wake-up for console/tty readiness in ppoll instead of the 20 ms re-check;
  `MSG_PEEK`, `sendmsg`/`recvmsg`.
* `/etc/hosts`; mDNS name probing and conflict resolution (RFC 6762 8).
* IPv6: privacy addresses (RFC 8981); MLD source filtering and MLDv1 compatibility;
  IGMP for IPv4 (still no multicast router on these links); PMTU black-hole detection
  (RFC 4821) for paths that drop ICMP.
* Keepalive; ECN; a smarter congestion controller (CUBIC); D-SACK; RACK-TLP.
* A routing table proper (today: per-link subnets, link-local/multicast to eth1, the
  default gateway on eth0); IGMP if a multicast router ever appears.
* Milk Drop: resumable transfers, several files or a folder per offer, a rotation chain
  longer than one step.
