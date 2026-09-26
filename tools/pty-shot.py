#!/usr/bin/env python3
"""Drive the TUI inside a pty and dump what a terminal would show."""
import os
import pty
import re
import select
import struct
import sys
import termios
import fcntl
import time

BIN = sys.argv[1] if len(sys.argv) > 1 else "target/release/omarchy-sysinfo"
KEYS = sys.argv[2] if len(sys.argv) > 2 else "jjj"
ROWS, COLS = 30, int(os.environ.get("COLS", 110))
WAIT = float(os.environ.get("WAIT", 1.0))

pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"
    os.execv(BIN, [BIN])

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

out = b""


def pump(seconds):
    global out
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.1)
        if r:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            out += chunk


pump(WAIT)
for key in KEYS:
    os.write(fd, key.encode())
    pump(0.4)
pump(0.5)
os.write(fd, b"q")
pump(0.5)
os.close(fd)
os.waitpid(pid, 0)

# A tiny terminal emulator: enough of CSI handling to rebuild the screen.
screen = [[" "] * COLS for _ in range(ROWS)]
row = col = 0
text = out.decode("utf-8", "replace")
i = 0
while i < len(text):
    ch = text[i]
    if ch == "\x1b":
        m = re.match(r"\x1b\[([0-9;?]*)([A-Za-z])", text[i:])
        if m:
            params, cmd = m.group(1), m.group(2)
            nums = [int(p) for p in params.split(";") if p.isdigit()]
            if cmd == "H":
                row = (nums[0] - 1) if nums else 0
                col = (nums[1] - 1) if len(nums) > 1 else 0
            elif cmd == "J":
                if not nums or nums[0] == 2:
                    screen = [[" "] * COLS for _ in range(ROWS)]
            elif cmd == "K":
                for c in range(col, COLS):
                    screen[row][c] = " "
            elif cmd == "C":
                col += nums[0] if nums else 1
            elif cmd == "D":
                col -= nums[0] if nums else 1
            i += m.end()
            continue
        m = re.match(r"\x1b\][^\x07\x1b]*(\x07|\x1b\\)", text[i:])
        if m:
            i += m.end()
            continue
        m = re.match(r"\x1b[()][A-Za-z0-9]", text[i:])
        if m:
            i += m.end()
            continue
        i += 1
        continue
    if ch == "\n":
        row += 1
        col = 0
    elif ch == "\r":
        col = 0
    elif ch == "\t":
        col = (col // 8 + 1) * 8
    elif ch >= " ":
        if 0 <= row < ROWS and 0 <= col < COLS:
            screen[row][col] = ch
        col += 1
    i += 1

for line in screen:
    print("".join(line).rstrip())
