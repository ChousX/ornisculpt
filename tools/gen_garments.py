#!/usr/bin/env python3
"""Writes assets/garments/garments.gltf: sample garments in wrap space.

Wrap space (see src/clothing/mod.rs): a unit cylinder around +Y stands for one body part.
y 0..1 runs over the garment's span, the angle around Y is the angle around the part (-Z = back,
+X = left/outer), and r - 1 is the height above the skin in metres. y < 0 hangs below the span.

Mesh0 tube    plain sleeve/tunic/trouser tube with rolled hems
Mesh1 bracer  tube with raised rims and lacing ridges
Mesh2 belt    band with a buckle at the front
Mesh3 skirt   band over the span, hanging below it with a scalloped hem
Mesh4 cape    back panel over the span, hanging below it
Mesh5 shorts  band over the span, capping the part's start (y -1..0 wraps over the end)

Run from the repo root: python3 tools/gen_garments.py
"""
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from gen_sample_parts import smooth_normals, write_gltf  # noqa: E402


def sheet(ys, thetas, height, ring):
    """Grid over (y rows, theta columns) at radius 1 + height(y, theta); triangles face outward."""
    cols = len(thetas)
    pos = []
    for y in ys:
        for t in thetas:
            r = 1.0 + height(y, t)
            pos.append((r * math.sin(t), y, -r * math.cos(t)))
    v = lambda i, j: i * cols + (j % cols)
    idx = []
    last = cols if ring else cols - 1
    for i in range(len(ys) - 1):
        for j in range(last):
            a, b, c, d = v(i, j), v(i + 1, j), v(i, j + 1), v(i + 1, j + 1)
            idx += [a, b, c, b, d, c]
    return pos, smooth_normals(pos, idx), idx


def ring_thetas(n):
    return [2 * math.pi * j / n for j in range(n)]


def rows(lo, hi, n):
    return [lo + (hi - lo) * i / n for i in range(n + 1)]


def tube():
    hem = lambda y: 0.003 * max(0.0, 1.0 - min(y, 1.0 - y) / 0.04)
    return sheet(rows(0.0, 1.0, 24), ring_thetas(40), lambda y, t: hem(y), True)


def bracer():
    def h(y, t):
        rim = 0.006 * max(0.0, 1.0 - min(y, 1.0 - y) / 0.1)
        lace = 0.002 * max(0.0, math.cos(14 * math.pi * y)) * max(0.0, math.cos(t - math.pi)) ** 8
        return 0.004 + rim + lace
    return sheet(rows(0.0, 1.0, 20), ring_thetas(40), h, True)


def belt():
    def h(y, t):
        buckle = 0.012 * max(0.0, 1.0 - abs(math.remainder(t - math.pi, 2 * math.pi)) / 0.22) ** 0.5
        return 0.005 + buckle * (0.2 < y < 0.8)
    return sheet(rows(0.0, 1.0, 10), ring_thetas(64), h, True)


def skirt():
    n = 48
    thetas = ring_thetas(n)
    ys = rows(-1.0, 1.0, 24)
    pos, _, idx = sheet(ys, thetas, lambda y, t: 0.002 + 0.004 * (y > 0.85), True)
    # Scalloped hem: pull the bottom row up between scallops.
    for j, t in enumerate(thetas):
        x, y, z = pos[j]
        pos[j] = (x, y + 0.12 * (1 - abs(math.cos(4 * t))), z)
    return pos, smooth_normals(pos, idx), idx


def cape():
    span = math.radians(75)
    thetas = [-span + 2 * span * j / 20 for j in range(21)]
    return sheet(rows(-1.0, 1.0, 24), thetas, lambda y, t: 0.004 + 0.003 * (y > 0.9), False)


def shorts():
    return sheet(rows(-1.0, 1.0, 24), ring_thetas(40), lambda y, t: 0.003 * (y > 0.92), True)


if __name__ == "__main__":
    out = Path("assets/garments/garments.gltf")
    write_gltf([("tube", tube()), ("bracer", bracer()), ("belt", belt()), ("skirt", skirt()), ("cape", cape()), ("shorts", shorts())], out)
    print(f"wrote {out}")
