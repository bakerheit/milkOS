#!/usr/bin/env python3
"""Drive a running qemu over QMP: type keys, move/click the pointer, take screenshots.

Usage: scripts/qmp.py SOCKET COMMAND [ARGS] [COMMAND [ARGS] ...]

SOCKET is a qemu `-qmp unix:SOCKET,server,nowait` socket. Commands run in order:

  type TEXT          type TEXT on a US layout ("\\n" in TEXT is Enter; shift added as needed)
  key COMBO          press one key combo by qemu qcode name, e.g. ret, shift-a, ctrl-c, f1
  hold KEY           press KEY (a qcode, e.g. meta_l, alt, shift) and keep it down
  release KEY        let it go (hold meta_l click release meta_l = a Cmd-click)
  move X Y           put the absolute pointer (virtio-tablet) on screen pixel X, Y
  button BTN STATE   BTN = left|right|middle, STATE = down|up
  click [BTN]        press and release BTN (default left)
  scroll N           wheel N notches (positive = up)
  sleep SECS         wait
  delay SECS         change the pause after each input command from here on
  screendump FILE    save the screen as PNG (as PPM if FILE ends in .ppm)
  hmp CMDLINE        run a human-monitor command, print its output
  raw JSON           send one QMP command object, print the reply

Options (before SOCKET): --screen WxH (pointer coordinate space, default 1024x768),
--delay SECS (pause after each input command, default 0.05).

Python 3 stdlib only. Example:
  scripts/qmp.py /tmp/q.sock type 'hello' key ret move 100 200 click screendump /tmp/s.png
"""

import json
import socket
import sys
import time

ABS_MAX = 0x7FFF  # qemu's absolute axis range is 0..ABS_MAX

# US layout: char -> (qcode, shift)
_PLAIN = {
    " ": "spc", "\n": "ret", "\t": "tab", "-": "minus", "=": "equal",
    "[": "bracket_left", "]": "bracket_right", "\\": "backslash", ";": "semicolon",
    "'": "apostrophe", "`": "grave_accent", ",": "comma", ".": "dot", "/": "slash",
}
_SHIFTED = {
    "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8",
    "(": "9", ")": "0", "_": "minus", "+": "equal", "{": "bracket_left",
    "}": "bracket_right", "|": "backslash", ":": "semicolon", '"': "apostrophe",
    "~": "grave_accent", "<": "comma", ">": "dot", "?": "slash",
}
# Friendly aliases for the `key` command.
_ALIASES = {"enter": "ret", "space": "spc", "esc": "esc", "backspace": "backspace",
            "bs": "backspace", "cmd": "meta_l", "meta": "meta_l", "super": "meta_l",
            "control": "ctrl"}


def char_keys(c):
    if c.isascii() and c.isalpha():
        return (["shift", c.lower()] if c.isupper() else [c])
    if c.isascii() and c.isdigit():
        return [c]
    if c in _PLAIN:
        return [_PLAIN[c]]
    if c in _SHIFTED:
        return ["shift", _SHIFTED[c]]
    raise SystemExit(f"qmp.py: cannot type {c!r} on a US layout")


class Qmp:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(path)
        self.sock.settimeout(10)
        self.buf = b""
        self._read()  # greeting
        self.cmd("qmp_capabilities")

    def _read(self):
        while b"\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise SystemExit("qmp.py: qemu closed the socket")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def send(self, obj):
        self.sock.sendall(json.dumps(obj).encode() + b"\n")
        while True:
            msg = self._read()
            if "event" in msg:
                continue
            if "error" in msg:
                raise SystemExit(f"qmp.py: {obj.get('execute')}: {msg['error']}")
            return msg.get("return")

    def cmd(self, name, **args):
        obj = {"execute": name}
        if args:
            obj["arguments"] = args
        return self.send(obj)

    def keys(self, names, hold_ms=30):
        self.cmd("send-key", keys=[{"type": "qcode", "data": k} for k in names],
                 **{"hold-time": hold_ms})
        # send-key returns at once but qemu replays the presses from a timer queue (hold time
        # plus a per-key delay). Wait it out, so keys stay in order with pointer events.
        time.sleep(hold_ms / 1000 + 0.015 * len(names) + 0.02)

    def events(self, evs):
        self.cmd("input-send-event", events=evs)


def main(argv):
    screen = (1024, 768)
    delay = 0.05
    while argv and argv[0].startswith("--"):
        opt = argv.pop(0)
        if opt == "--screen":
            w, h = argv.pop(0).lower().split("x")
            screen = (int(w), int(h))
        elif opt == "--delay":
            delay = float(argv.pop(0))
        else:
            raise SystemExit(f"qmp.py: unknown option {opt}")
    if len(argv) < 2:
        raise SystemExit(__doc__)
    q = Qmp(argv.pop(0))

    def to_abs(v, size):
        # Centre of pixel v, so the guest's v * size / (ABS_MAX + 1) lands back on v.
        return min(ABS_MAX, (2 * v + 1) * (ABS_MAX + 1) // (2 * size))

    def btn(name, down):
        q.events([{"type": "btn", "data": {"button": name, "down": down}}])

    while argv:
        op = argv.pop(0)
        if op == "type":
            for c in argv.pop(0):
                q.keys(char_keys(c))
                time.sleep(delay)
        elif op == "key":
            q.keys([_ALIASES.get(k, k) for k in argv.pop(0).split("-")])
            time.sleep(delay)
        elif op in ("hold", "release"):
            k = argv.pop(0)
            k = _ALIASES.get(k, k)
            q.events([{"type": "key", "data": {"down": op == "hold", "key": {"type": "qcode", "data": k}}}])
            time.sleep(delay)
        elif op == "move":
            x, y = int(argv.pop(0)), int(argv.pop(0))
            q.events([
                {"type": "abs", "data": {"axis": "x", "value": to_abs(x, screen[0])}},
                {"type": "abs", "data": {"axis": "y", "value": to_abs(y, screen[1])}},
            ])
            time.sleep(delay)
        elif op == "button":
            b, state = argv.pop(0), argv.pop(0)
            btn(b, state == "down")
            time.sleep(delay)
        elif op == "click":
            b = argv.pop(0) if argv and argv[0] in ("left", "right", "middle") else "left"
            btn(b, True)
            time.sleep(delay)
            btn(b, False)
            time.sleep(delay)
        elif op == "scroll":
            n = int(argv.pop(0))
            name = "wheel-up" if n > 0 else "wheel-down"
            for _ in range(abs(n)):
                btn(name, True)
                btn(name, False)
                time.sleep(delay)
        elif op == "sleep":
            time.sleep(float(argv.pop(0)))
        elif op == "delay":
            delay = float(argv.pop(0))
        elif op == "screendump":
            f = argv.pop(0)
            q.cmd("screendump", filename=f, format="ppm" if f.endswith(".ppm") else "png")
        elif op == "hmp":
            out = q.cmd("human-monitor-command", **{"command-line": argv.pop(0)})
            if out:
                print(out, end="")
        elif op == "raw":
            print(json.dumps(q.send(json.loads(argv.pop(0)))))
        else:
            raise SystemExit(f"qmp.py: unknown command {op!r}")


if __name__ == "__main__":
    main(sys.argv[1:])
