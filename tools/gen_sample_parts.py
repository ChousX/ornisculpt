#!/usr/bin/env python3
"""Writes assets/parts/sample_parts.gltf: test meshes for authored parts.

Mesh0: a watertight three-toed foot (for Sculpt parts). Origin at the ankle, Y up, +Z forward.
Mesh1: a saw-tooth crest (for rigid HardKind::Mesh parts). Origin at its base, Y up, +Z forward.

Run from the repo root: python3 tools/gen_sample_parts.py
"""
import base64
import json
import math
import struct
from pathlib import Path


def foot(rings=24, sides=48):
    """Flattened ellipsoid pad with three toe lobes in front. Triangles wind outward (CCW)."""
    rx, ry, rz = 0.05, 0.028, 0.07
    center = (0.0, -0.03, 0.035)
    pos = [(center[0], center[1] + ry, center[2])]  # north pole
    for i in range(1, rings):
        phi = math.pi * i / rings
        for j in range(sides):
            a = 2 * math.pi * j / sides  # 0 = forward (+Z)
            dx, dy, dz = math.sin(phi) * math.sin(a), math.cos(phi), math.sin(phi) * math.cos(a)
            toes = max(0.0, math.cos(5 * a)) ** 3 * max(0.0, math.cos(a)) ** 0.5
            bump = 1.0 + 0.6 * toes * (1.0 - abs(dy)) ** 2
            pos.append((center[0] + dx * rx * bump, center[1] + dy * ry, center[2] + dz * rz * bump))
    pos.append((center[0], center[1] - ry, center[2]))  # south pole
    south = len(pos) - 1
    ring = lambda i, j: 1 + (i - 1) * sides + j % sides
    idx = []
    for j in range(sides):
        idx += [0, ring(1, j), ring(1, j + 1)]
    for i in range(1, rings - 1):
        for j in range(sides):
            a, b, c, d = ring(i, j), ring(i, j + 1), ring(i + 1, j), ring(i + 1, j + 1)
            idx += [a, c, b, b, c, d]
    for j in range(sides):
        idx += [south, ring(rings - 1, j + 1), ring(rings - 1, j)]
    normals = smooth_normals(pos, idx)
    return pos, normals, idx


def crest():
    """Saw-tooth profile in the YZ plane, extruded along X (flat-shaded prism)."""
    profile = [(0.0, -0.06), (0.05, -0.04), (0.02, -0.02), (0.07, 0.0), (0.03, 0.02), (0.06, 0.05), (0.0, 0.06)]
    half = 0.006
    pos, nrm, idx = [], [], []

    def quad(p0, p1, p2, p3, n):
        base = len(pos)
        pos.extend([p0, p1, p2, p3])
        nrm.extend([n] * 4)
        idx.extend([base, base + 1, base + 2, base, base + 2, base + 3])

    # Side faces (fan triangulation of the concave profile is fine for this simple shape).
    for side, sx in ((1, half), (-1, -half)):
        base = len(pos)
        for y, z in profile:
            pos.append((sx, y, z))
            nrm.append((float(side), 0.0, 0.0))
        for k in range(1, len(profile) - 1):
            tri = [base, base + k, base + k + 1]
            idx.extend(tri if side > 0 else [tri[0], tri[2], tri[1]])
    # Edge faces around the profile.
    for k in range(len(profile)):
        (y0, z0), (y1, z1) = profile[k], profile[(k + 1) % len(profile)]
        ny, nz = z1 - z0, -(y1 - y0)
        ln = math.hypot(ny, nz) or 1.0
        quad((half, y0, z0), (-half, y0, z0), (-half, y1, z1), (half, y1, z1), (0.0, ny / ln, nz / ln))
    return pos, nrm, idx


def smooth_normals(pos, idx):
    acc = [[0.0, 0.0, 0.0] for _ in pos]
    for t in range(0, len(idx), 3):
        a, b, c = (pos[i] for i in idx[t : t + 3])
        u = [b[k] - a[k] for k in range(3)]
        v = [c[k] - a[k] for k in range(3)]
        n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]
        for i in idx[t : t + 3]:
            for k in range(3):
                acc[i][k] += n[k]
    out = []
    for n in acc:
        ln = math.sqrt(sum(x * x for x in n)) or 1.0
        out.append(tuple(x / ln for x in n))
    return out


def write_gltf(meshes, path):
    blob = bytearray()
    views, accessors, gl_meshes = [], [], []

    def add(data, fmt, count, target, acc_type, comp, extra=None):
        while len(blob) % 4:
            blob.append(0)
        offset = len(blob)
        blob.extend(struct.pack(f"<{len(data)}{fmt}", *data))
        views.append({"buffer": 0, "byteOffset": offset, "byteLength": len(blob) - offset, "target": target})
        acc = {"bufferView": len(views) - 1, "componentType": comp, "count": count, "type": acc_type}
        acc.update(extra or {})
        accessors.append(acc)
        return len(accessors) - 1

    for name, (pos, nrm, idx) in meshes:
        flat = [c for p in pos for c in p]
        mins = [min(p[k] for p in pos) for k in range(3)]
        maxs = [max(p[k] for p in pos) for k in range(3)]
        p_acc = add(flat, "f", len(pos), 34962, "VEC3", 5126, {"min": mins, "max": maxs})
        n_acc = add([c for n in nrm for c in n], "f", len(nrm), 34962, "VEC3", 5126)
        i_acc = add(idx, "I", len(idx), 34963, "SCALAR", 5125)
        gl_meshes.append({"name": name, "primitives": [{"attributes": {"POSITION": p_acc, "NORMAL": n_acc}, "indices": i_acc}]})

    doc = {
        "asset": {"version": "2.0", "generator": "ornisculpt gen_sample_parts.py"},
        "buffers": [{"byteLength": len(blob), "uri": "data:application/octet-stream;base64," + base64.b64encode(bytes(blob)).decode()}],
        "bufferViews": views,
        "accessors": accessors,
        "meshes": gl_meshes,
        "nodes": [{"name": m["name"], "mesh": i, "translation": [i * 0.3, 0, 0]} for i, m in enumerate(gl_meshes)],
        "scenes": [{"nodes": list(range(len(gl_meshes)))}],
        "scene": 0,
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(doc))


if __name__ == "__main__":
    out = Path("assets/parts/sample_parts.gltf")
    write_gltf([("foot", foot()), ("crest", crest())], out)
    print(f"wrote {out}")
