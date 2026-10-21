# Sourced by run.sh / screenshot.sh: qemu user-mode networking for eth0 (virtio-net).
#   milk_net_args   -> NET_ARGS
# restrict=on: the guest gets DHCP (10.0.2.15) and can ping the gateway 10.0.2.2, but
# cannot reach the host or the internet (no router/DNS handed out either). Port
# forwards still work: MILK_HTTP_PORT=8088 adds hostfwd 127.0.0.1:8088 -> guest :80
# (opt-in, so parallel qemus don't fight over the port). scripts/net-test.sh sets up its
# own, unrestricted, netdev for the host-loopback tests.
# MILK_V6ONLY=1: a v6-only boot. slirp runs without IPv4 (ipv4=off) and the kernel is told
# (fw_cfg opt/milkos/net) to keep IPv4 off eth0 and eth1. MILK_HTTP_PORT doesn't apply then:
# this qemu's hostfwd only forwards IPv4.
milk_net_args() {
  local nd="user,id=n0,restrict=on"
  if [ "${MILK_V6ONLY:-}" = 1 ]; then
    NET_ARGS=(-netdev "$nd,ipv4=off" -device virtio-net-device,netdev=n0
      -fw_cfg "name=opt/milkos/net,string=v6only=1")
    return
  fi
  [ -n "${MILK_HTTP_PORT:-}" ] && nd="$nd,hostfwd=tcp:127.0.0.1:${MILK_HTTP_PORT}-:80"
  NET_ARGS=(-netdev "$nd" -device virtio-net-device,netdev=n0)
}
