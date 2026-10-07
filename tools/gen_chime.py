#!/usr/bin/env python3
"""Synthesises the bundled chimes (a soft two-note "ding-dong") as small mono WAV files.

Run from the repository root: python3 tools/gen_chime.py
Output: data/sounds/chime.wav (focus end) and data/sounds/chime-low.wav (break end variant).
"""

import math
import os
import struct
import wave

RATE = 16000
LENGTH = 1.1  # seconds


def bell(freq, t):
    """A bell-like partial mix with a gentle attack and exponential decay."""
    if t < 0:
        return 0.0
    attack = min(1.0, t / 0.012)
    decay = math.exp(-t * 3.2)
    partials = (
        1.0 * math.sin(2 * math.pi * freq * t)
        + 0.35 * math.sin(2 * math.pi * freq * 2.0 * t) * math.exp(-t * 4.0)
        + 0.12 * math.sin(2 * math.pi * freq * 3.01 * t) * math.exp(-t * 7.0)
    )
    return attack * decay * partials


def render(path, high, low):
    n = int(RATE * LENGTH)
    samples = []
    for i in range(n):
        t = i / RATE
        v = 0.55 * bell(high, t) + 0.5 * bell(low, t - 0.32)
        # Fade the tail to silence so playback ends cleanly.
        tail = min(1.0, (LENGTH - t) / 0.15)
        samples.append(max(-1.0, min(1.0, 0.45 * v * tail)))
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(b"".join(struct.pack("<h", int(s * 32767)) for s in samples))


def main():
    os.makedirs("data/sounds", exist_ok=True)
    render("data/sounds/chime.wav", 987.77, 783.99)  # B5 -> G5
    render("data/sounds/chime-low.wav", 659.25, 523.25)  # E5 -> C5


if __name__ == "__main__":
    main()
