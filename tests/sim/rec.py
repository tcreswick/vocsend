#!/usr/bin/env python3
# Stand-in microphone: real-time samples from the peer's tape, plus our own echo and noise.
import os, sys, time
import numpy as np
T0 = float(os.environ["AIR_T0"]); peer = open(os.environ["AIR_IN"], "rb"); own = open(os.environ["AIR_OUT"], "rb")
noise = float(os.environ.get("AIR_NOISE", "0.003")); rng = np.random.default_rng()
def grab(f, idx):
    f.seek(idx * 2); b = f.read(3200); x = np.frombuffer(b[: len(b) // 2 * 2], "<i2").astype(np.float32) / 32768
    return np.concatenate([x, np.zeros(1600 - len(x), np.float32)])
idx = int((time.time() - T0) * 16000)
while True:
    wait = T0 + (idx + 1600) / 16000 + 0.05 - time.time()
    if wait > 0: time.sleep(wait)
    burst = os.path.exists(os.environ['AIR_OUT'] + '.burst')
    x = 0.5 * grab(peer, idx) + 0.3 * grab(own, idx) + rng.normal(0, 0.6 if burst else noise, 1600)
    sys.stdout.buffer.write((np.clip(x, -1, 1) * 32767).astype("<i2").tobytes()); sys.stdout.buffer.flush()
    idx += 1600
