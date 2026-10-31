#!/usr/bin/env python3
"""Writes assets/parts/head_goblin.gltf: a goblin's head, the MakeHuman head (assets/parts/head_mh.gltf,
from tools/gen_head.py) warped by smooth displacement fields round its landmarks (tools/head_warp.py):

- a long, sharp nose drawn out to a point from a long bridge;
- big eyes set deep under a heavy brow, the forehead sloping back;
- cheekbones out and the cheeks hollow under them;
- a narrow jaw tapering to a sharp chin thrust forward, and a wide, thin mouth with the corners
  pulled back;
- the base drawn in to the thin neck;
- long ears drawn up to a point (the plan's own huge ears sit over them).

Prints the warped landmarks for the plan's `SculptHead`. Metres, Y up, +Z forward, X left.

    python3 tools/gen_head_goblin.py
"""
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from head_warp import EAR, EYE, MOUTH, NOSE, bump, generate, mirrored, scaled_about, smooth  # noqa: E402


def warp(p):
    x, y, z = p
    front = smooth(-0.01, 0.05, z)  # only the face, not the back of the skull
    # A long, sharp nose: drawn far out from its root along a long bridge, the last third
    # narrowing to a point and dipping a little, not a human hook.
    nose = bump(p, (0.0, 0.06, 0.085), (0.016, 0.02, 0.026))
    tip = bump(p, (0.0, 0.058, 0.09), (0.013, 0.013, 0.02))
    bridge = bump(mirrored(p), (0.0, 0.085, 0.082), (0.012, 0.018, 0.02))
    z += 0.03 * nose + 0.05 * tip + 0.006 * bridge
    y -= 0.006 * nose + 0.014 * tip
    y += (0.058 - p[1]) * 0.4 * tip
    x *= 1.0 - 0.2 * nose - 0.55 * tip
    # Big eyes set deep: each socket opened out about its eye's centre and sunk back into the
    # head, under a heavy brow ridge that stands out over it.
    for side in (1.0, -1.0):
        c = (side * EYE[0], EYE[1], EYE[2])
        w = bump(p, c, (0.02, 0.017, 0.017))
        x, y, z = scaled_about((x, y, z), c, 1.0 + 0.35 * w)
        z -= 0.012 * bump(p, c, (0.026, 0.022, 0.02))
    brow = bump(mirrored(p), (0.03, 0.114, 0.06), (0.04, 0.011, 0.03)) * front
    z += 0.012 * brow
    y -= 0.004 * brow
    fore = smooth(0.125, 0.19, p[1]) * (1.0 - smooth(0.19, 0.215, p[1])) * front
    z -= 0.012 * fore
    # Cheekbones out and forward as a shelf under each socket, the cheeks hollow under them.
    cheekbone = bump(mirrored(p), (0.05, 0.08, 0.045), (0.018, 0.012, 0.025))
    x += math.copysign(0.006, x) * cheekbone
    z += 0.006 * bump(mirrored(p), (0.036, 0.068, 0.058), (0.018, 0.01, 0.02))
    hollow = bump(mirrored(p), (0.05, 0.05, 0.045), (0.02, 0.018, 0.025))
    x -= math.copysign(0.005, x) * hollow
    # A narrow jaw tapering to a sharp chin, thrust forward and down.
    low = 1.0 - smooth(0.0, 0.07, p[1])
    side = smooth(0.0, 0.03, p[2] + 0.06)  # not behind the ears
    x *= 1.0 - 0.22 * low * side
    chin = bump(p, (0.0, 0.005, 0.07), (0.022, 0.02, 0.028))
    z += 0.022 * chin
    y -= 0.008 * chin
    x *= 1.0 - 0.45 * chin
    # A wide, thin mouth, the corners pulled back along the cheeks.
    mouth = bump(p, MOUTH, (0.03, 0.012, 0.025))
    x *= 1.0 + 0.4 * mouth
    y += (MOUTH[1] - p[1]) * 0.3 * mouth
    corner = bump(mirrored(p), (0.03, 0.037, 0.075), (0.012, 0.012, 0.015))
    z -= 0.01 * corner
    # Long ears drawn up and back to a point.
    ear = bump(mirrored(p), (0.082, 0.1, -0.03), (0.02, 0.05, 0.035)) * smooth(0.06, 0.078, abs(p[0]))
    x, y, z = scaled_about((x, y, z), (math.copysign(EAR[0], x), EAR[1], EAR[2]), (1.0, 1.0 + 0.6 * ear, 1.0 + 0.3 * ear))
    point = bump(mirrored(p), (0.085, 0.14, -0.025), (0.015, 0.02, 0.02))
    y += 0.02 * point
    z -= 0.01 * point
    # The base drawn in toward the thin neck, and the flat underside of the jaw sloped down to the
    # chin, so the head rounds into the neck instead of overhanging it with a square edge.
    base = 1.0 - smooth(0.0, 0.035, p[1])
    x, y, z = scaled_about((x, y, z), (0.0, y, -0.01), (1.0 - 0.2 * base, 1.0, 1.0 - 0.2 * base))
    under = (1.0 - smooth(0.0, 0.02, p[1])) * smooth(-0.02, 0.03, p[2])
    y -= 0.012 * under
    return (x, y, z)


if __name__ == "__main__":
    generate("goblin", warp)
