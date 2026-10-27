#!/usr/bin/env python3
"""Writes assets/parts/head_mh.gltf: a watertight human head for `PartKind::Sculpt` heads, cut
from the MakeHuman base mesh (CC0 1.0: github.com/makehumancommunity/makehuman,
makehuman/data/3dobjs/base.obj; the mesh is an asset, released CC0 by the MakeHuman team).

The head is the `body` group above the neck's narrowest slice, with the neck opening capped.
Metres (MakeHuman's decimetres ÷ 10), origin at the centre of the neck cut, Y up, +Z forward.
Also prints the eye centres (from the eye helper groups) and other landmarks relative to that
origin, for `SculptHead`.

    python3 tools/gen_head.py path/to/base.obj
"""
import itertools
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from gen_sample_parts import smooth_normals, write_gltf  # noqa: E402


def load(path):
    vs, groups, cur = [], {}, None
    for line in open(path):
        if line.startswith("v "):
            vs.append(tuple(map(float, line.split()[1:4])))
        elif line.startswith("g "):
            cur = line.split(None, 1)[1].strip()
        elif line.startswith("f "):
            groups.setdefault(cur, []).append([int(t.split("/")[0]) - 1 for t in line.split()[1:]])
    return vs, groups


def head(vs, groups):
    body = groups["body"]
    # The neck: the narrowest slice (by width) between the shoulders and the chin.
    best = None
    for k in range(60):
        y = 5.4 + k * 0.025
        width = max((abs(vs[i][0]) for f in body for i in f if abs(vs[i][1] - y) < 0.05), default=0.0)
        if width > 0 and (best is None or width < best[0]):
            best = (width, y)
    cut = best[1]
    faces = [f for f in body if all(vs[i][1] >= cut - 0.02 for i in f)]
    used = sorted(set(itertools.chain.from_iterable(faces)))
    remap = {v: i for i, v in enumerate(used)}
    # Boundary edges (used once) ring the openings: the neck, and the eye sockets (the eyes are
    # meshes of their own in MakeHuman). Each loop is capped with a fan to its centroid; the
    # neck's, the largest, sets the origin.
    edges = {}
    for f in faces:
        for a, b in zip(f, f[1:] + f[:1]):
            edges.setdefault((min(a, b), max(a, b)), []).append((a, b))
    boundary = {e: d[0] for e, d in edges.items() if len(d) == 1}
    nxt = {}
    for (a, b) in boundary.values():
        nxt[a] = b
    loops = []
    seen = set()
    for start in list(nxt):
        if start in seen:
            continue
        loop, v = [], start
        while v not in seen:
            seen.add(v)
            loop.append(v)
            v = nxt.get(v)
            if v is None:
                break
        loops.append(loop)
    loops.sort(key=len, reverse=True)
    neck = loops[0]
    cx = sum(vs[v][0] for v in neck) / len(neck)
    cy = sum(vs[v][1] for v in neck) / len(neck)
    cz = sum(vs[v][2] for v in neck) / len(neck)
    pos = [((vs[v][0] - cx) * 0.1, (vs[v][1] - cy) * 0.1, (vs[v][2] - cz) * 0.1) for v in used]
    idx = []
    for f in faces:
        q = [remap[v] for v in f]
        for t in range(1, len(q) - 1):
            idx.extend([q[0], q[t], q[t + 1]])
    # Caps, wound against the loop's own direction (the loop runs the way the faces wind, so the
    # cap faces outward when its triangles run the other way).
    for loop in loops:
        c = len(pos)
        pos.append(tuple(sum(pos[remap[v]][k] for v in loop) / len(loop) for k in range(3)))
        for a, b in zip(loop, loop[1:] + loop[:1]):
            idx.extend([c, remap[b], remap[a]])
    print(f"capped {len(loops)} openings: {[len(l) for l in loops]} vertices")
    # Winding check by signed volume: flip if inside out.
    vol = 0.0
    for t in range(0, len(idx), 3):
        a, b, c = (pos[i] for i in idx[t : t + 3])
        vol += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0])) / 6
    if vol < 0:
        idx = [i for t in range(0, len(idx), 3) for i in (idx[t], idx[t + 2], idx[t + 1])]
    eyes = {}
    for side in ("l", "r"):
        g = groups.get(f"helper-{side}-eye", [])
        pts = {i for f in g for i in f}
        if pts:
            eyes[side] = tuple(round((sum(vs[i][k] for i in pts) / len(pts) - (cx, cy, cz)[k]) * 0.1, 4) for k in range(3))
    lo = [min(p[k] for p in pos) for k in range(3)]
    hi = [max(p[k] for p in pos) for k in range(3)]
    r = lambda p: tuple(round(c, 4) for c in p)
    marks = dict(
        nose=r(max(pos, key=lambda p: p[2])),
        crown=r(max(pos, key=lambda p: p[1])),
        ear=r(max((p for p in pos if 0.07 < p[1] < 0.14), key=lambda p: p[0])),
        chin=r(min((p for p in pos if abs(p[0]) < 0.01 and p[2] > 0.0), key=lambda p: p[1])),
        mouth=r(min((p for p in pos if abs(p[0]) < 0.005 and 0.03 < p[1] < 0.08), key=lambda p: -p[2])),
    )
    return pos, idx, dict(cut=cut, verts=len(pos), tris=len(idx) // 3, volume=abs(vol), lo=lo, hi=hi, eyes=eyes, **marks)


if __name__ == "__main__":
    vs, groups = load(sys.argv[1])
    pos, idx, info = head(vs, groups)
    out = Path("assets/parts/head_mh.gltf")
    write_gltf([("head", (pos, smooth_normals(pos, idx), idx))], out)
    print(f"wrote {out}: {info}")
