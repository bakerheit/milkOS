# Sourced by run.sh / screenshot.sh: qemu user-mode networking for eth0 (virtio-net).
#   milk_net_args   -> NET_ARGS
# restrict=on: the guest gets DHCP (10.0.2.15) and can ping the gateway 10.0.2.2, but
# cannot reach the host or the internet (no router/DNS handed out either). Port
# forwards still work: MILK_HTTP_PORT=8088 adds hostfwd 127.0.0.1:8088 -> guest :80
# (opt-in, so parallel qemus don't fight over the port). scripts/net-test.sh sets up its
# own, unrestricted, netdev for the host-loopback tests.
milk_net_args() {
  local nd="user,id=n0,restrict=on"
  [ -n "${MILK_HTTP_PORT:-}" ] && nd="$nd,hostfwd=tcp:127.0.0.1:${MILK_HTTP_PORT}-:80"
  NET_ARGS=(-netdev "$nd" -device virtio-net-device,netdev=n0)
}
