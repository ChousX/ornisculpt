#!/usr/bin/env python3
"""Writes assets/parts/head_gnome.gltf: a gnome's head, the MakeHuman head (assets/parts/head_mh.gltf,
from tools/gen_head.py) warped by smooth displacement fields round its landmarks (tools/head_warp.py):

- a big, round, bulbous nose, drooping a little at the tip;
- a heavy brow, the eyes small under it;
- full, round cheeks and a short, soft lower face;
- a round, broad cranium;
- large ears standing out from the head.

Prints the warped landmarks for the plan's `SculptHead`. Metres, Y up, +Z forward, X left.

    python3 tools/gen_head_gnome.py
"""
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from head_warp import EAR, EYE, bump, generate, mirrored, scaled_about, smooth  # noqa: E402


def warp(p):
    x, y, z = p
    front = smooth(-0.01, 0.05, z)  # only the face, not the back of the skull
    # A bulbous nose: the whole nose swells out from a centre behind its tip, rounder than long,
    # and the tip droops.
    bulb = (0.0, 0.058, 0.08)
    d = tuple(p[k] - bulb[k] for k in range(3))
    r = math.sqrt(sum(c * c for c in d))
    nose = math.exp(-((r / 0.024) ** 2))
    if r > 1e-6:
        x += d[0] / r * 0.02 * nose
        y += d[1] / r * 0.01 * nose
        z += d[2] / r * 0.018 * nose
    x *= 1.0 + 0.35 * nose
    y -= 0.006 * nose
    # A heavy brow, out over the eyes.
    brow = bump(mirrored(p), (0.03, 0.112, 0.06), (0.04, 0.011, 0.03)) * front
    z += 0.009 * brow
    y -= 0.003 * brow
    # Small eyes: each socket drawn in toward its eye's centre.
    for side in (1.0, -1.0):
        c = (side * EYE[0], EYE[1], EYE[2])
        w = bump(p, c, (0.02, 0.016, 0.016))
        x, y, z = scaled_about((x, y, z), c, 1.0 - 0.18 * w)
    # Full cheeks: round pads under the cheekbones, out and forward, sagging a little.
    cheek = bump(mirrored(p), (0.046, 0.055, 0.05), (0.022, 0.022, 0.028))
    x += math.copysign(0.012, x) * cheek
    z += 0.006 * cheek
    y -= 0.003 * cheek
    # A short lower face: everything below the eyes lifted toward them at the front.
    y += max(0.09 - p[1], 0.0) * 0.06 * front
    # A round cranium, a touch wider.
    cranium = smooth(0.1, 0.16, p[1])
    x *= 1.0 + 0.06 * cranium
    # Large ears: scaled up about the root and stood out from the head.
    ear = bump(mirrored(p), (0.082, 0.095, -0.03), (0.02, 0.045, 0.035)) * smooth(0.06, 0.078, abs(p[0]))
    x, y, z = scaled_about((x, y, z), (math.copysign(EAR[0], x), EAR[1], EAR[2]), (1.0, 1.0 + 0.7 * ear, 1.0 + 0.6 * ear))
    x += math.copysign(0.014, x) * ear
    return (x, y, z)


if __name__ == "__main__":
    generate("gnome", warp)
