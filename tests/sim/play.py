#!/usr/bin/env python3
# Stand-in speaker: writes what it is given onto a shared "tape" at the current time.
import os, sys, time
T0 = float(os.environ["AIR_T0"]); f = open(os.environ["AIR_OUT"], "r+b"); pos = 0; carry = b""
while True:
    b = os.read(0, 6400)
    if not b: break
    b = carry + b; carry = b[len(b) // 2 * 2:]; b = b[: len(b) // 2 * 2]
    now = int((time.time() - T0) * 16000); pos = max(pos, now)
    f.seek(pos * 2); f.write(b); f.flush(); pos += len(b) // 2
    ahead = (pos - now) / 16000
    if ahead > 0.3: time.sleep(ahead - 0.3)
