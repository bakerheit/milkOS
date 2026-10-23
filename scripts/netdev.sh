# Sourced by run.sh / screenshot.sh / wm-demo.sh / the tests: qemu user-mode networking for eth0
# (virtio-net).
#   milk_net_args            -> NET_ARGS, offline: what every test gets
#   milk_net_args online     -> NET_ARGS, the internet on (scripts/run.sh), unless MILK_OFFLINE=1
#
# Offline (restrict=on): the guest gets DHCP (10.0.2.15) and can ping the gateway 10.0.2.2, but
# cannot reach the host or the internet (no router/DNS handed out either), and the kernel isn't
# told about any internet, so it keeps its offline policy (docs/networking.md, "Internet").
# Online: slirp without restrict=on (NAT to the Mac's network, DNS at 10.0.2.3 forwarding to the
# Mac's resolver) and fw_cfg opt/milkos/net says internet=on, so the guest resolves names, the
# Browser visits them and Settings > Network says "Internet: on".
# Under scripts/check-all.sh (MILK_CHECK_ALL=1) asking for online is an error: tests stay
# offline (check-all also puts a qemu wrapper first in PATH that refuses an internet=on boot).
# Port forwards work either way: MILK_HTTP_PORT=8088 adds hostfwd 127.0.0.1:8088 -> guest :80
# (opt-in, so parallel qemus don't fight over the port). scripts/net-test.sh and web-test.sh set
# up their own, local-only netdevs.
# MILK_V6ONLY=1: a v6-only boot. slirp runs without IPv4 (ipv4=off) and the kernel is told
# (fw_cfg opt/milkos/net) to keep IPv4 off eth0 and eth1. MILK_HTTP_PORT doesn't apply then:
# this qemu's hostfwd only forwards IPv4.
milk_net_args() {
  local online=0
  if [ "${1:-}" = online ] && [ "${MILK_OFFLINE:-}" != 1 ]; then
    online=1
    if [ "${MILK_CHECK_ALL:-}" = 1 ]; then
      echo "netdev.sh: INTERNET GUARD: a boot under scripts/check-all.sh asked for the internet;" \
        "tests must stay offline (MILK_OFFLINE=1, or milk_net_args without 'online')" >&2
      exit 97
    fi
  fi
  local nd="user,id=n0" words=""
  [ "$online" = 1 ] && words="internet=on" || nd="$nd,restrict=on"
  if [ "${MILK_V6ONLY:-}" = 1 ]; then
    NET_ARGS=(-netdev "$nd,ipv4=off" -device virtio-net-device,netdev=n0
      -fw_cfg "name=opt/milkos/net,string=v6only=1${words:+ $words}")
    return
  fi
  [ -n "${MILK_HTTP_PORT:-}" ] && nd="$nd,hostfwd=tcp:127.0.0.1:${MILK_HTTP_PORT}-:80"
  NET_ARGS=(-netdev "$nd" -device virtio-net-device,netdev=n0)
  [ -n "$words" ] && NET_ARGS+=(-fw_cfg "name=opt/milkos/net,string=$words")
  return 0
}
