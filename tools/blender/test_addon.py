"""Headless check of the Ornisculpt add-on: on an exported body, add a part from a template, mark
a mesh as a sculpt modelled in place and another as a garment, then export with the add-on.

    blender -b --python tools/blender/test_addon.py -- in.body.glb out.body.glb

`convert` on the result should show the new part, the sculpt (its mesh from the file) and the
garment (its part inferred).
"""
import sys
import os

import bpy

sys.path.insert(0, os.path.dirname(__file__))
import ornisculpt  # noqa: E402

args = sys.argv[sys.argv.index("--") + 1 :]
src, out = args

bpy.ops.wm.read_factory_settings(use_empty=True)
ornisculpt.register()
bpy.ops.import_scene.gltf(filepath=src)

torso = next(o for o in bpy.data.objects if ornisculpt.is_part(o) and o.parent is not None and not ornisculpt.is_part(o.parent))
view = bpy.context.view_layer


def activate(obj, *others):
    for o in bpy.context.selected_objects:
        o.select_set(False)
    for o in (obj, *others):
        o.select_set(True)
    view.objects.active = obj


labels = [t["label"] for t in ornisculpt.templates()["parts"]]
horn = next(i for i, l in enumerate(labels) if l.startswith("Hard Spike") or l.startswith("Hard Horn"))
activate(torso)
assert bpy.ops.ornisculpt.add_part(template=str(horn)) == {"FINISHED"}
print("test_addon: added", labels[horn], "as", bpy.context.active_object.name)

# A small closed lump on the back, marked as a sculpt of the torso.
bpy.ops.mesh.primitive_ico_sphere_add(radius=0.04, location=torso.matrix_world.translation + __import__("mathutils").Vector((0, 0.1, 0.12)))
lump = bpy.context.active_object
lump.name = "lump"
activate(torso, lump)
assert bpy.ops.ornisculpt.mark_part(kind="Sculpt") == {"FINISHED"}
print("test_addon: marked lump as a sculpt of", ornisculpt.part_path(torso))

# A loose band around the torso, marked as a garment (its part inferred).
bpy.ops.mesh.primitive_cylinder_add(radius=0.22, depth=0.12, location=torso.matrix_world.translation, end_fill_type="NOTHING")
band = bpy.context.active_object
band.name = "band"
activate(band)
assert bpy.ops.ornisculpt.mark_garment() == {"FINISHED"}
print("test_addon: marked band as a garment")

bpy.context.scene.ornisculpt_path = out
assert bpy.ops.ornisculpt.export() == {"FINISHED"}
print("test_addon: wrote", out)
