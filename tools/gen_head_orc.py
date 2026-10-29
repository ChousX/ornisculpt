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
import base64
import json
import math
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from gen_sample_parts import smooth_normals, write_gltf  # noqa: E402

# The MakeHuman head's landmarks (SculptHead's defaults), left side.
EYE = (0.0308, 0.0945, 0.0488)
NOSTRIL = (0.011, 0.05, 0.078)
EAR = (0.085, 0.09, -0.038)
NOSE = (0.0, 0.057, 0.092)
MOUTH = (0.0, 0.037, 0.082)
CROWN = (0.0, 0.2153, -0.006)


def read_gltf(path):
    doc = json.loads(Path(path).read_text())
    blob = base64.b64decode(doc["buffers"][0]["uri"].split(",", 1)[1])

    def accessor(i, fmt, width):
        acc = doc["accessors"][i]
        view = doc["bufferViews"][acc["bufferView"]]
        start = view.get("byteOffset", 0) + acc.get("byteOffset", 0)
        n = acc["count"] * width
        data = struct.unpack_from(f"<{n}{fmt}", blob, start)
        return [tuple(data[k : k + width]) for k in range(0, n, width)] if width > 1 else list(data)

    prim = doc["meshes"][0]["primitives"][0]
    pos = accessor(prim["attributes"]["POSITION"], "f", 3)
    idx = accessor(prim["indices"], "I", 1)
    return pos, idx


def bump(p, centre, sigma):
    """Gaussian falloff from `centre`, per-axis widths `sigma`."""
    return math.exp(-sum(((p[k] - centre[k]) / sigma[k]) ** 2 for k in range(3)))


def smooth(e0, e1, x):
    t = min(max((x - e0) / (e1 - e0), 0.0), 1.0)
    return t * t * (3 - 2 * t)


def warp(p):
    x, y, z = p
    front = smooth(-0.01, 0.05, z)  # only the face, not the back of the skull
    # Brow ridge: a bar across both eyes, out and a little down.
    brow = bump((abs(x), y, z), (0.03, 0.113, 0.06), (0.04, 0.011, 0.03)) * front
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
    flare = bump((abs(x), y, z), (0.014, 0.05, 0.075), (0.008, 0.008, 0.012))
    x += math.copysign(0.004, x) * flare
    # Cheekbones out.
    cheek = bump((abs(x), y, z), (0.05, 0.083, 0.045), (0.018, 0.014, 0.025))
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


def landmark(p):
    return tuple(round(c, 4) for c in warp(p))


if __name__ == "__main__":
    pos, idx = read_gltf("assets/parts/head_mh.gltf")
    out = [warp(p) for p in pos]
    path = Path("assets/parts/head_orc.gltf")
    write_gltf([("head_orc", (out, smooth_normals(out, idx), idx))], path)
    print(f"wrote {path}")
    for name, p in [("eye", EYE), ("nostril", NOSTRIL), ("ear", EAR), ("nose", NOSE), ("mouth", MOUTH), ("crown", CROWN)]:
        print(f"{name}: {landmark(p)}")
