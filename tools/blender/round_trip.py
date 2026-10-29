"""Blender round trip of a *.body.glb: import, apply edits, export.

Checks that Blender keeps Ornisculpt's extras (custom properties) through its glTF importer and
exporter, and gives `convert` edits to bring back:

    blender -b --python tools/blender/round_trip.py -- in.body.glb out.body.glb [edit ...]

Edits: `move:<object>:<metres>` lifts an object (glTF +Y, Blender +Z); `scale:<object>:<factor>`
scales a mesh's vertices about their centre (loosening or tightening a garment, or resizing a
part's mesh without touching its node).
"""
import json
import sys

import bpy

args = sys.argv[sys.argv.index("--") + 1 :]
src, out, edits = args[0], args[1], args[2:]

bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.import_scene.gltf(filepath=src)

tagged = [o for o in bpy.data.objects if "ornisculpt" in o.keys()]
parts = [o.name for o in tagged if "part" in o["ornisculpt"].keys()]
garments = [o.name for o in tagged if "garment" in o["ornisculpt"].keys()]
print(f"round_trip: {len(parts)} part objects: {', '.join(parts)}")
print(f"round_trip: {len(garments)} garment objects: {', '.join(garments)}")
if parts:
    print("round_trip: first part's extras:", json.dumps(bpy.data.objects[parts[0]]["ornisculpt"].to_dict())[:300])

for edit in edits:
    kind, name, amount = edit.split(":")
    o = bpy.data.objects[name]
    if kind == "move":
        # Blender is Z up: +Z here is glTF's +Y.
        o.matrix_world.translation.z += float(amount)
    elif kind == "scale":
        verts = o.data.vertices
        centre = sum((v.co for v in verts), verts[0].co * 0) / len(verts)
        for v in verts:
            v.co = centre + (v.co - centre) * float(amount)
    else:
        raise SystemExit(f"round_trip: unknown edit {edit}")
    print(f"round_trip: {edit}")

bpy.ops.export_scene.gltf(filepath=out, export_format="GLB", export_extras=True, export_yup=True)
print(f"round_trip: wrote {out}")
