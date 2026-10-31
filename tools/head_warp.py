"""Shared code for the heads warped from the MakeHuman head (assets/parts/head_mh.gltf, from
tools/gen_head.py): tools/gen_head_orc.py, gen_head_gnome.py and gen_head_goblin.py each define a
`warp(p)` over smooth displacement fields round the head's landmarks and hand it to `generate`,
which writes assets/parts/head_<name>.gltf and prints the warped landmarks for the plan's
`SculptHead`. Metres, Y up, +Z forward, X left.
"""
import base64
import json
import math
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from gen_sample_parts import smooth_normals, write_gltf  # noqa: E402

SOURCE = "assets/parts/head_mh.gltf"

# The MakeHuman head's landmarks (SculptHead's defaults), left side.
EYE = (0.0308, 0.0945, 0.0488)
NOSTRIL = (0.011, 0.05, 0.078)
EAR = (0.085, 0.09, -0.038)
NOSE = (0.0, 0.057, 0.092)
MOUTH = (0.0, 0.037, 0.082)
CROWN = (0.0, 0.2153, -0.006)
LANDMARKS = [("eye", EYE), ("nostril", NOSTRIL), ("ear", EAR), ("nose", NOSE), ("mouth", MOUTH), ("crown", CROWN)]


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


def mirrored(p):
    """`p` folded onto the left side, for fields symmetric about the midline."""
    return (abs(p[0]), p[1], p[2])


def smooth(e0, e1, x):
    t = min(max((x - e0) / (e1 - e0), 0.0), 1.0)
    return t * t * (3 - 2 * t)


def scaled_about(p, centre, factor):
    """`p` scaled by `factor` (one number, or one per axis) about `centre`."""
    f = factor if isinstance(factor, tuple) else (factor,) * 3
    return tuple(centre[k] + (p[k] - centre[k]) * f[k] for k in range(3))


def generate(name, warp):
    """Writes assets/parts/head_<name>.gltf, the source head through `warp`, and prints the warped
    landmarks as the plan's `sculpt` fields."""
    pos, idx = read_gltf(SOURCE)
    out = [warp(p) for p in pos]
    path = Path(f"assets/parts/head_{name}.gltf")
    write_gltf([(f"head_{name}", (out, smooth_normals(out, idx), idx))], path)
    print(f"wrote {path}")
    for label, p in LANDMARKS:
        x, y, z = (0.0 if abs(c) < 5e-5 else round(c, 4) for c in warp(p))
        print(f"{label}: ({x}, {y}, {z}),")
