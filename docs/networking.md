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
* **Memory.** The device's rings and buffers and the socket buffers (32 sockets x 2 x 16 KiB,
  one contiguous PMM block) are allocated once at boot. Frames are built straight into
  device tx buffers; received frames are parsed in place in their rx buffer.
* **Loopback**: 127/8 and our own address go into a 32-packet ring that `with` drains after
  the call (bounded), so a loopback connect/accept/transfer completes inside the calls.

## Protocols

* **virtio-net**: features MAC + STATUS only (no offloads, no mergeable buffers, no control
  queue). 12-byte `virtio_net_hdr` (VERSION_1), sent zeroed. Up to 64 rx and 64 tx
  descriptors, 2 KiB buffer each.
* **Ethernet / ARP**: per-interface 32-entry cache, requests rate-limited to 1/s, 3 tries; up to 8 packets
  wait (3 s) for an answer, then are dropped. Entries live 10 minutes. The gateway is
  resolved right after DHCP.
* **IPv4**: header checksum checked, DF set on everything we send, TTL 64. Fragments (MF or
  offset) are dropped and counted (`ReasmFails`), we never fragment (UDP > 1472 bytes:
  `EMSGSIZE`). Routing: loopback, on-link (per interface), link-local and multicast to
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
* **TCP**: RFC 9293 state machine (all 11 states), active and passive open, simultaneous
  open, MSS option (we announce 1460, default 536), sliding window (16 KiB each way, no
  window scaling needed), retransmission timer per RFC 6298 (RTO from SRTT/RTTVAR, min
  200 ms, max 60 s, Karn's rule, exponential backoff that is reset by new ACKs), slow start /
  congestion avoidance / fast retransmit on 3 duplicate ACKs, zero-window probes (persist
  timer), out-of-order segments kept in the receive ring past its readable end (up to 8
  holes) and committed when the hole fills, FIN in every state, RST rules (RFC 5961
  challenge ACKs for in-window RST/SYN), RST for segments to closed ports, RST instead of FIN
  when closing with unread data (as Linux). Limits: SYN retried 5 times (~31 s), data 10
  times, then `ETIMEDOUT` (+RST). TIME_WAIT is 2 s (2 x MSL of 1 s; Linux: 60 s), orphaned
  FIN_WAIT_2 30 s. No SACK, no timestamps, no delayed ACK, no Nagle, no keepalive, no
  urgent data.

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
pub fn socket_icmp() -> Result<SocketId>                        // ping socket (Dgram, ICMP echo; see the ping row below)
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
| 198 | socket | `AF_INET` only (`EAFNOSUPPORT`); `SOCK_STREAM`/`SOCK_DGRAM` + `SOCK_NONBLOCK`/`SOCK_CLOEXEC`; protocol 0/6/17, or `SOCK_DGRAM` + 1 (`IPPROTO_ICMP`) for a ping socket |
| 200 | bind | `struct sockaddr_in` (16 bytes) |
| 201 | listen | |
| 202 / 242 | accept / accept4 | blocks unless non-blocking; accept4 flags `SOCK_NONBLOCK`/`SOCK_CLOEXEC` |
| 203 | connect | blocks until connected or failed unless non-blocking (`EINPROGRESS`) |
| 204 / 205 | getsockname / getpeername | |
| 206 / 207 | sendto / recvfrom | `MSG_DONTWAIT`; blocking send queues everything before returning |
| 208 / 209 | setsockopt / getsockopt | set: SO_RCVTIMEO (recv/recvfrom/read/accept give EAGAIN after it); SO_REUSEADDR, SO_KEEPALIVE, SO_SNDBUF, SO_RCVBUF, SO_REUSEPORT, SO_SNDTIMEO, TCP_NODELAY accepted and ignored; get: SO_ERROR, SO_TYPE; else `ENOPROTOOPT` |
| 210 | shutdown | |
| 73 | ppoll | any fd: sockets report real readiness, the console is readable once a line (or ^D) is typed, other files are always ready (`Inode::poll`, default POLLIN\|POLLOUT). Sleeps on the stack's readiness queue, re-checking non-socket fds every 20 ms. The sigmask is ignored. |

Not yet: `select`/`epoll`, `sendmsg`/`recvmsg`, `socketpair`, `MSG_PEEK`, `AF_UNIX`, signals interrupting a blocking call (there are no signals yet).
`read`/`write` on a socket honour `O_NONBLOCK` as of the last socket syscall on that fd
(`OpenFile` does not pass its flags to the inode; the socket syscalls re-sync them).

## /proc/net

`dev` (lo, eth0 counters), `arp`, `route` (Linux hex format), `tcp` (Linux columns, plus a
`# STATE rexmit N rto Nms` comment per line), `udp`, `snmp` (Ip/Icmp/Tcp/Udp counters),
and milkOS's `ifaddr` (`eth0 flags=UP,BROADCAST,RUNNING mtu=1500 ether=.. inet=..
netmask=.. broadcast=.. gateway=.. dns=.. dhcp=Bound`).

## Userland tools (`user/`)

| program | what | how |
|---|---|---|
| `ifconfig [IFACE]` | addresses, MAC, gateway/DNS/DHCP state, counters | `/proc/net/ifaddr` (milkOS, `key=value` per interface) + `/proc/net/dev` |
| `netstat [-t] [-u] [-l] [-s]` | sockets, or protocol counters | `/proc/net/{tcp,udp,snmp}` |
| `ping [-c N] [-i MS] [-s SIZE] [-W MS] ADDR` | ICMP echo, Linux-style output | a **ping socket**: `socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP)` as on Linux (unprivileged). `sendto` an echo request (the kernel sets the identifier = the socket's local "port", and the checksum), `recvfrom` gets the replies for that identifier (ICMP header + data, no IP header, so no TTL), `SO_RCVTIMEO` for the timeout. Default 4 pings (no ^C yet). |
| `nc [-u] HOST PORT`, `nc -l [-u] PORT` | stdin ↔ socket | `ppoll` on fd 0 and the socket; ^D half-closes (TCP) |
| `httpd [-p PORT] [-q] [DIR]` | static files over HTTP/1.0 (GET, HEAD), index.html or a listing for directories, `..` refused (403), 301 for a directory without `/` | started by init as `httpd -q /srv/www` on :80 (rootfs/srv/www) |
| `/status` (httpd) | a live dashboard: uptime, load, CPU per core, memory, network totals, the 15 newest audit events | generated per request from /proc, in the HTML subset the desktop Browser renders (no tables or CSS: bars are text in `<pre>`); `open http://127.0.0.1/status` |
| `fetch [-i] URL` | HTTP GET | |

Addresses are numeric (or `localhost`): there is no resolver for user programs yet.

## In-kernel HTTP server (demo, off by default)

`net/services.rs`, on the net thread, using only the socket API: `/` (HTML with the
interface config), `/net` (all of /proc/net), `/big` (1 MiB of numbered 32-byte lines), 404
otherwise. HTTP/1.0, `Connection: close`, 8 connections at a time. Off unless
`net::services::enable(port)` is called; the self-test does that with `khttpd=PORT` in its
fw_cfg config (net-test.sh uses 8080, for the `/big` window/retransmission checks).

## Tests

**Boot self-test** (`net::selftest`, after the sched self-test, prints `[net] selftest ok`):
DHCP (3 s timeout), 3 pings to the gateway, UDP over loopback, 256 KiB + 64 KiB TCP over
loopback with an orderly close both ways, **64 KiB over a lossy loopback** (every 7th packet
dropped: data, ACKs, SYN and FIN) to prove retransmission and reassembly, connection
refused (RST), an HTTP GET to our own server, and the DNS policy. With the fw_cfg file
`opt/milkos/nettest` (`echo=IP:PORT dns=IP:PORT name=NAME expect=IP refused=IP:PORT`, passed
by net-test.sh) also: 512 KiB TCP echo against the Mac, a `.test` lookup + NXDOMAIN against
the Mac's test DNS server, a refused host port. It then prints /proc/net.

**`scripts/net-test.sh`**: starts `scripts/net_test_servers.py` (TCP echo, test DNS, HTTP,
all on 127.0.0.1, from a mktemp dir), boots with
hostfwds for guest :80 (userland httpd), :8080 (kernel httpd) and :7777 (nc -l), checks
the self-test, types `ifconfig`, `ping`, three `fetch`es, `nc` to the host echo server (then
^D), `nc -l 7777` (the Mac connects and sends a line) and `netstat` into msh over the serial
console (`scripts/serial_drive.py`), then from the Mac: the userland httpd (`/`, a file, a
listing, 404, 403 for `..`, 301) and the kernel httpd (`/big` content verified, 4 parallel
`/big`, `/net`, 404). Kills only the qemu and
python processes it started, by PID.

## Next

* `epoll`; a real wake-up for console/tty readiness in ppoll instead of the 20 ms re-check;
  `MSG_PEEK`, `sendmsg`/`recvmsg`.
* A resolver for user programs (an `/etc/hosts` + the same `.test` policy, or a syscall).
* Per-socket buffer sizes instead of the fixed 16 KiB pool; window scaling then.
* Delayed ACKs and Nagle (fewer packets), SACK, IP fragment reassembly, keepalive.
* Multiple interfaces (the routing table is three rules today).
