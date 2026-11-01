"""Synthesizes the listening tone as a 48 kHz mono 16-bit WAV file in the
directory given as the first argument, along with pitch variants for
comparison. data/sounds holds the chosen one as FLAC:

    gst-launch-1.0 filesrc location=prompt.wav ! wavparse ! audioconvert \\
        ! flacenc ! filesink location=data/sounds/listening-prompt.flac
"""
import sys
import wave

import numpy as np

RATE = 48000
OUT = sys.argv[1]
# A soft sound is heard over and over in a conversation, so it peaks well
# below full scale.
PEAK_DB = -12


def note(freq, length, attack, decay):
    n = int(length * RATE)
    t = np.arange(n) / RATE
    # A faint octave that fades within 15 ms rounds the onset without adding
    # a lasting edge.
    body = np.sin(2 * np.pi * freq * t) + 0.12 * np.sin(4 * np.pi * freq * t) * np.exp(-t / 0.015)
    envelope = np.minimum(t / attack, 1.0) * np.exp(-np.maximum(t - attack, 0) / decay)
    return body * envelope


def prompt(low, high):
    first = note(low, 0.11, 0.008, 0.045)
    second = note(high, 0.20, 0.008, 0.065)
    gap = int(0.075 * RATE)
    both = np.zeros(gap + len(second))
    both[: len(first)] += first * 0.8
    both[gap:] += second
    return both


def save(name, signal):
    signal = np.concatenate([signal, np.zeros(int(0.03 * RATE))])
    signal = signal / np.max(np.abs(signal)) * 10 ** (PEAK_DB / 20)
    fade = int(0.004 * RATE)
    signal[-fade:] *= np.linspace(1, 0, fade)
    with wave.open(f"{OUT}/{name}.wav", "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes((signal * 32767).astype("<i2").tobytes())


# A rising fifth. The shipped tone is the lowest pitch.
save("prompt-d5", prompt(587.33, 880.00))
save("prompt-c5", prompt(523.25, 783.99))
save("prompt", prompt(440.00, 659.25))
