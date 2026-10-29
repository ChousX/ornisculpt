"""Blender round trip of a *.body.glb: import, optionally move one part up, export.

Checks that Blender keeps Ornisculpt's extras (custom properties) through its glTF importer and
exporter, and gives `convert` a moved part to place.

    blender -b --python tools/blender/round_trip.py -- in.body.glb out.body.glb [part] [metres]
"""
import json
import sys

import bpy

args = sys.argv[sys.argv.index("--") + 1 :]
src, out = args[0], args[1]
part = args[2] if len(args) > 2 else None
lift = float(args[3]) if len(args) > 3 else 0.02

bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.import_scene.gltf(filepath=src)

parts = [o for o in bpy.data.objects if "ornisculpt" in o.keys() and "part" in o["ornisculpt"].keys()]
print(f"round_trip: {len(parts)} part objects: {', '.join(o.name for o in parts)}")
sample = parts[0]["ornisculpt"].to_dict()
print("round_trip: first part's extras:", json.dumps(sample)[:400])

if part:
    o = bpy.data.objects[part]
    # Blender is Z up: +Z here is glTF's +Y.
    o.matrix_world.translation.z += lift
    print(f"round_trip: moved {part} up {lift} m")

bpy.ops.export_scene.gltf(filepath=out, export_format="GLB", export_extras=True, export_yup=True)
print(f"round_trip: wrote {out}")
