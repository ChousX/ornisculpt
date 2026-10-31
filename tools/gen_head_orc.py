#!/usr/bin/env python3
"""Writes assets/parts/head_orc.gltf: an orc's head, the MakeHuman head (assets/parts/head_mh.gltf,
from tools/gen_head.py) warped by smooth displacement fields round its landmarks:

- a heavy brow ridge standing out over the eyes, the forehead sloping back above it;
- a broad, flat nose, flared at the nostrils;
- heavy cheekbones and a wide, deep, square jaw with the chin pushed forward;
- a wide mouth, the lower lip thrust out (the tusks come up past it).

Prints the warped landmarks for the plan's `SculptHead`. Metres, Y up, +Z forward, X left.

    python3 tools/gen_head_orc.py
"""
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from head_warp import MOUTH, NOSE, bump, generate, mirrored, smooth  # noqa: E402


def warp(p):
    x, y, z = p
    front = smooth(-0.01, 0.05, z)  # only the face, not the back of the skull
    # Brow ridge: a bar across both eyes, out and a little down.
    brow = bump(mirrored((x, y, z)), (0.03, 0.113, 0.06), (0.04, 0.011, 0.03)) * front
    z += 0.013 * brow
    y -= 0.004 * brow
    # Forehead sloping back above the brow.
    fore = smooth(0.125, 0.19, y) * (1.0 - smooth(0.19, 0.215, y)) * front
    z -= 0.012 * fore
    # Broad, flat nose: wider, the tip pressed back and down, the nostrils flared.
    nose = bump(p, (0.0, 0.06, 0.088), (0.024, 0.022, 0.03))
    x *= 1.0 + 0.55 * nose
    z -= 0.009 * bump(p, NOSE, (0.012, 0.014, 0.02))
    y -= 0.004 * nose
    flare = bump(mirrored((x, y, z)), (0.014, 0.05, 0.075), (0.008, 0.008, 0.012))
    x += math.copysign(0.004, x) * flare
    # Cheekbones out.
    cheek = bump(mirrored((x, y, z)), (0.05, 0.083, 0.045), (0.018, 0.014, 0.025))
    x += math.copysign(0.007, x) * cheek
    # A wide, deep jaw: the lower face widens below the cheeks and drops a little.
    low = 1.0 - smooth(0.0, 0.075, y)
    side = smooth(0.0, 0.03, z + 0.06)  # not behind the ears
    x *= 1.0 + 0.22 * low * side
    y -= 0.008 * low * front
    # The chin square and forward.
    chin = bump(p, (0.0, 0.008, 0.07), (0.03, 0.02, 0.03))
    z += 0.012 * chin
    x *= 1.0 + 0.25 * chin
    # A wide mouth, the lower lip thrust out.
    mouth = bump(p, MOUTH, (0.03, 0.012, 0.025))
    x *= 1.0 + 0.18 * mouth
    lower_lip = bump(p, (0.0, 0.03, 0.08), (0.02, 0.006, 0.015))
    z += 0.006 * lower_lip
    return (x, y, z)


if __name__ == "__main__":
    generate("orc", warp)
