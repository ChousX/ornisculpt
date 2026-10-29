#!/usr/bin/env python3
"""Writes assets/parts/horns.gltf: horns for rigid HardKind::Mesh parts.

Mesh0: a unicorn's horn, 0.55 m: a slender cone twisted with two spiral ridges, as a narwhal's
tusk is. Origin at its base, +Y along the horn.

Run from the repo root: python3 tools/gen_horns.py
"""
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from gen_sample_parts import smooth_normals, write_gltf  # noqa: E402


def spiral_horn(length=0.55, base=0.034, turns=4.5, ridges=2, depth=0.13, rings=90, sides=28):
    """Tapered tube whose section is a circle with `ridges` lobes turning `turns` times base to tip."""
    pos, idx = [], []
    for i in range(rings + 1):
        t = i / rings
        radius = base * (1.0 - t) ** 0.85 + 0.0015
        # The ridges flatten out toward the tip.
        d = depth * (1.0 - t) ** 0.5
        phase = 2 * math.pi * turns * t
        for j in range(sides):
            a = 2 * math.pi * j / sides
            r = radius * (1.0 - d + d * math.cos(ridges * (a - phase)))
            pos.append((r * math.cos(a), length * t, r * math.sin(a)))
    ring = lambda i, j: i * sides + j % sides
    for i in range(rings):
        for j in range(sides):
            a, b, c, d = ring(i, j), ring(i, j + 1), ring(i + 1, j), ring(i + 1, j + 1)
            idx += [a, c, b, b, c, d]
    # Tip and base caps.
    tip = len(pos)
    pos.append((0.0, length + 0.004, 0.0))
    for j in range(sides):
        idx += [ring(rings, j), tip, ring(rings, j + 1)]
    root = len(pos)
    pos.append((0.0, -0.002, 0.0))
    for j in range(sides):
        idx += [ring(0, j), ring(0, j + 1), root]
    return pos, smooth_normals(pos, idx), idx


if __name__ == "__main__":
    out = Path("assets/parts/horns.gltf")
    write_gltf([("unicorn_horn", spiral_horn())], out)
    print(f"wrote {out}")
