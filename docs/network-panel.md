# Settings > Network: design note

For whoever builds the panel (the wm side). The networking side gives the data and the
controls. The panel only reads text and runs `network`; it never parses kernel structures
or writes network files itself. Everything here exists and is tested (`scripts/net-test.sh`,
`scripts/drop-test.sh`).

## What it shows

One window, three groups.

**This Mac** (the machine as others see it):

| field | example | from | notes |
|---|---|---|---|
| Hostname | `milk` | `hostname=` | uname's nodename. Editable (admin). |
| Local name | `bob.local` | `local_fqdn=` | The Milk Drop name plus `.local`, answered by mDNS on the VM link. Editable (admin). Empty if the machine has no Milk Drop identity yet (the drop service makes one at first boot). |
| Visible to nearby Macs | on | `drop_visible=on\|off` | A switch (admin). Off: no discovery announcements, no answers to `drop list` or `.local` lookups. Transfers from peers that already know the address still work. |

**Interfaces**, one card each, in the order `network status --keys` gives them (`lo`, `eth0`, `eth1`).
Show `lo` collapsed or not at all.

| field | key (`iface.NAME.`) | example | notes |
|---|---|---|---|
| Status | `flags` | `UP,BROADCAST,RUNNING` | "Connected" if it contains `RUNNING`, else "No link". |
| Kind | (name) | | `eth0` = "Ethernet (host network)", `eth1` = "VM link (Milk Drop)". |
| MAC | `mac` | `52:54:00:12:34:56` | |
| MTU | `mtu` | `1500` | |
| IPv4 | `ipv4` | `10.0.2.15/24` | `off`: IPv4 is off on this link (a v6-only boot), so show "Off". Absent: "No IPv4 address". |
| Configured by | `dhcp` / `linklocal` | `Bound` / `bound` | eth0: DHCP state `Off`, `Selecting`, `Requesting`, `Bound`, `Renewing` ("Bound" = "DHCP"; the others = "Getting an address..."). eth1: `probing` / `bound` ("Self-assigned"). Neither key: IPv4 is off on that link (a v6-only boot). |
| Router | `gateway` | `10.0.2.2` | |
| DNS | `dns` | `10.0.2.3` | Informational: user programs resolve only numbers, `localhost` and `.local` names. |
| IPv6 | `ipv6` | `fe80::5054:ff:fe12:3456/64,link fec0::5054:ff:fe12:3456/64,global` | Space-separated. Each item is `ADDR/LEN,SCOPE[,tentative]`, with SCOPE `link` or `global`. Show one per line: link-local ones as "Link-local", global ones as "Address". `tentative` means duplicate address detection is still running (half a second after the link comes up), so grey it out. |
| IPv6 router | `router6` | `fe80::2` | |

**Advanced** (a disclosure triangle, read-only): the raw `/proc/net` files the panel can
show as text: `ifaddr`, `if_inet6`, `route`, `ndisc` (IPv6 neighbours), `arp`, `pmtu`
(learned path MTUs), `igmp6` (multicast groups). Plain text, monospaced.

## Where the data comes from

Run `network status --keys`. It prints stable `key=value` lines, one per field, and the panel splits each
line at the first `=`:

```text
hostname=milk
local_name=bob
local_fqdn=bob.local
drop_visible=on
iface.eth0.flags=UP,BROADCAST,RUNNING
iface.eth0.mtu=1500
iface.eth0.mac=52:54:00:12:34:56
iface.eth0.gateway=10.0.2.2
iface.eth0.dns=10.0.2.3
iface.eth0.dhcp=Bound
iface.eth0.router6=fe80::2
iface.eth0.ipv4=10.0.2.15/24
iface.eth0.ipv6=fe80::5054:ff:fe12:3456/64,link fec0::5054:ff:fe12:3456/64,global
iface.eth1.linklocal=bound
...
```

The rules:

- Keys are only ever added, never renamed. A missing key means "none" or "not applicable".
- Values never contain `=`; only `ipv6` contains spaces.
- The command only reads a few small files, so it's cheap. Refresh every 2 s while the window is open and
  visible, at once after any change the panel makes, and on focus. Nothing pushes changes
  to the panel; addresses change on their own (DHCP, SLAAC, link-local probing,
  duplicate address detection).

Kernel side, `network` reads `/proc/net/ifaddr` and `/proc/net/hostname`; the Milk Drop
side, `/System/Settings/milk/drop` (`name`, `visible`). The panel shouldn't read those directly: their
formats are the kernel's and the daemon's, and `network status --keys` is the contract.

## The controls

Each control is one `network` command. Exit status 0 means done. Anything else: show the
command's stderr line, minus its `network VERB: ` prefix (`network name: `, `network hide: `), under the field, and keep the old
value.

| control | command | who | errors to expect |
|---|---|---|---|
| Hostname | `network name NAME` | root | `only root can change the hostname`; `NAME can't be a hostname (1-63 letters, digits and '-')` |
| Local name | `network name --local NAME` (with or without `.local`) | administrators (root, group `admin`) | `bad name ...: 1-32 letters, digits, '.', '_' or '-'`; `only an administrator may change this machine's identity`; `the Milk Drop service isn't running` |
| Visible | `network show` / `network hide` | administrators | `only an administrator ...`; `the Milk Drop service isn't running` |

- **Validate while typing**, with the same rules: hostname `^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?$`,
  local name 1-32 of `[A-Za-z0-9._-]`. The command still decides.
- **Who may change what.** The desktop account (milk) is an administrator, so name and
  visibility work without a password. Setting the hostname needs root: run it through the
  system's usual elevation (`admin network name NAME` asks for the password; show the
  sheet the desktop uses elsewhere for that). Don't grey a control out just because the
  user isn't root; let the command refuse.
- **Persistence.** All three survive a reboot on a machine with a Milk volume:
  `/System/Settings/milk/net/hostname`, which the kernel applies at boot, and `/System/Settings/milk/drop/name` and
  `visible`. Without a volume they last until shutdown. The panel can say "Changes are kept
  on the Milk volume" when `/Volumes/Milk` is mounted.
- **Audit.** Every change is logged: `net.hostname`, `drop.rename`, `drop.visible`, with
  the uid. The panel needn't do anything for that.

## States to design for

- **No VM link** (no `iface.eth1.*` keys). Hide the local name and visibility rows, or show
  them disabled with "Milk Drop needs the VM link".
- **Milk Drop service down.** The name and visibility commands fail with `the Milk Drop
  service isn't running`. Show that once, under the group, not per field.
- **Hidden** (`drop_visible=off`). Say what it means in one line: "Other Macs won't see
  this one or find bob.local. Transfers from Macs that already know it still work."
- **IPv6 only** (a v6-only boot: `ipv4=off`, no `dhcp`/`linklocal` keys). Show IPv4 as
  "Off" rather than "No address", to tell it apart from a DHCP failure.
- **Getting an address** (`dhcp` not `Bound`, or `linklocal=probing`, or `tentative` IPv6).
  Show a spinner next to the field, not an error.

## Not in this panel (yet)

There is no static IPv4 configuration, no DNS server setting (user programs don't use
one), no Wi-Fi, and no per-interface enable/disable. Ask the network side if the panel
needs one of these; each would be a new `network` verb with the same contract.
