"""Ornisculpt: author body plans, parts and garments in Blender (docs/design/interchange.md).

Import a `*.body.glb` made by the `export` example (File > Import > glTF). Its part empties carry
their part in the `ornisculpt` custom property. This add-on adds, in the Object properties:

- Add part: a part from a template (lifted from the sample plans), nested under the active part.
- Mark as part: the selected mesh becomes a part modelled in place (a sculpt or a mesh hard part)
  of the active part.
- Mark as garment: the selected mesh becomes a garment, modelled on the body in its real shape.

and in the Scene properties the file to export to, with an option to export on every save: a
running `editor` or `viewer` on that file hot-reloads it.

Plain glTF with hand-typed custom properties works the same; this only makes it quicker.
"""

import copy
import json
import os

import bpy
from bpy.app.handlers import persistent

bl_info = {
    "name": "Ornisculpt",
    "author": "Ornisculpt",
    "version": (0, 1, 0),
    "blender": (4, 2, 0),
    "location": "Properties > Object / Scene > Ornisculpt",
    "description": "Author Ornisculpt body plans, parts and garments",
    "category": "Import-Export",
}

KEY = "ornisculpt"
_templates = None


def templates():
    """The part and garment templates (templates.json, from the `templates` example)."""
    global _templates
    if _templates is None:
        path = os.path.join(os.path.dirname(__file__), "templates.json")
        with open(path, encoding="utf-8") as f:
            _templates = json.load(f)
    return _templates


def data(obj):
    """An object's `ornisculpt` property as plain data, or {}."""
    if obj is None or KEY not in obj.keys():
        return {}
    value = obj[KEY]
    return value.to_dict() if hasattr(value, "to_dict") else dict(value)


def is_part(obj):
    return "part" in data(obj)


def is_garment(obj):
    return "garment" in data(obj)


def part_name(obj):
    """A part object's name in the plan, as import reads it: the object's name, except that the
    `.001` Blender appends to keep names unique across the file doesn't count."""
    name = data(obj).get("part", {}).get("name", obj.name)
    suffix = obj.name[len(name) + 1 :] if obj.name.startswith(name + ".") else None
    return name if obj.name == name or (suffix and suffix.isdigit()) else obj.name


def part_path(obj):
    """The part path of a part object, from its part ancestors' names (`torso/arm`)."""
    names = []
    while obj is not None:
        if is_part(obj):
            names.append(part_name(obj))
        obj = obj.parent
    return "/".join(reversed(names))


def unique_name(name, parent):
    """`name`, or with `_1`, `_2`... when a sibling part already has it."""
    siblings = parent.children if parent is not None else []
    taken = {part_name(o) for o in siblings if is_part(o)}
    out, n = name, 1
    while out in taken:
        out = f"{name}_{n}"
        n += 1
    return out


def add_part_tree(context, node, parent):
    """Empties for `node` and its children, `node`'s under `parent`; returns `node`'s."""
    children = node.pop("children", [])
    node["name"] = unique_name(node["name"], parent)
    empty = bpy.data.objects.new(node["name"], None)
    empty.empty_display_type = "ARROWS"
    empty.empty_display_size = 0.03
    context.collection.objects.link(empty)
    empty[KEY] = {"part": node}
    if parent is not None:
        empty.parent = parent
    for child in children:
        add_part_tree(context, child, empty)
    return empty


def template_items(self, context):
    return [(str(i), t["label"], t["from"]) for i, t in enumerate(templates()["parts"])]


class ORNISCULPT_OT_add_part(bpy.types.Operator):
    """Add a part from a template under the active part (its children too)"""

    bl_idname = "ornisculpt.add_part"
    bl_label = "Add Part"
    bl_options = {"REGISTER", "UNDO"}
    bl_property = "template"

    template: bpy.props.EnumProperty(name="Template", items=template_items)

    @classmethod
    def poll(cls, context):
        return is_part(context.active_object)

    def invoke(self, context, event):
        context.window_manager.invoke_search_popup(self)
        return {"RUNNING_MODAL"}

    def execute(self, context):
        parent = context.active_object
        node = copy.deepcopy(templates()["parts"][int(self.template)]["part"])
        empty = add_part_tree(context, node, parent)
        # It has no frame: import places it where it is. Start it at its parent, a little out.
        empty.matrix_world = parent.matrix_world.copy()
        empty.location.z += 0.02
        for o in context.selected_objects:
            o.select_set(False)
        empty.select_set(True)
        context.view_layer.objects.active = empty
        return {"FINISHED"}


class ORNISCULPT_OT_mark_part(bpy.types.Operator):
    """Make the selected meshes parts modelled in place, nested under the active part"""

    bl_idname = "ornisculpt.mark_part"
    bl_label = "Mark as Part"
    bl_options = {"REGISTER", "UNDO"}

    kind: bpy.props.EnumProperty(
        name="Kind",
        items=[
            ("Sculpt", "Sculpt", "Blended into the body's surface (closed mesh)"),
            ("Mesh", "Mesh", "A rigid mesh on the part's bone (horn, plate, gear)"),
        ],
    )

    @classmethod
    def poll(cls, context):
        return is_part(context.active_object) and any(o.type == "MESH" and o != context.active_object for o in context.selected_objects)

    def execute(self, context):
        parent = context.active_object
        for o in context.selected_objects:
            if o == parent or o.type != "MESH":
                continue
            node = copy.deepcopy(templates()["modelled"][self.kind])
            node["name"] = unique_name(o.name, parent)
            node.pop("children", None)
            o[KEY] = {"part": node}
            world = o.matrix_world.copy()
            o.parent = parent
            o.matrix_world = world
        return {"FINISHED"}


class ORNISCULPT_OT_mark_garment(bpy.types.Operator):
    """Make the selected meshes garments; with a part active, they wrap that part"""

    bl_idname = "ornisculpt.mark_garment"
    bl_label = "Mark as Garment"
    bl_options = {"REGISTER", "UNDO"}

    @classmethod
    def poll(cls, context):
        return any(o.type == "MESH" and not is_part(o) for o in context.selected_objects)

    def execute(self, context):
        active = context.active_object
        on = part_path(active) if is_part(active) else None
        for o in context.selected_objects:
            if o.type != "MESH" or is_part(o):
                continue
            garment = copy.deepcopy(templates()["garment"])
            garment["name"] = o.name
            # Which part it wraps, and over what stretch, come from where it lies unless given.
            for k in ("on", "span", "mesh"):
                garment.pop(k, None)
            if on:
                garment["on"] = on
            o[KEY] = {"garment": garment}
        return {"FINISHED"}


def export(scene):
    path = bpy.path.abspath(scene.ornisculpt_path)
    if not path:
        return False
    bpy.ops.export_scene.gltf(filepath=path, export_format="GLB", export_extras=True, export_yup=True)
    return True


class ORNISCULPT_OT_export(bpy.types.Operator):
    """Export the body (glTF binary, custom properties included) to the Ornisculpt file"""

    bl_idname = "ornisculpt.export"
    bl_label = "Export Body"

    def execute(self, context):
        if not export(context.scene):
            self.report({"ERROR"}, "Set the file to export to first")
            return {"CANCELLED"}
        self.report({"INFO"}, f"Exported {context.scene.ornisculpt_path}")
        return {"FINISHED"}


@persistent
def export_on_save(_):
    scene = bpy.context.scene
    if scene is not None and scene.ornisculpt_live:
        export(scene)


class ORNISCULPT_PT_object(bpy.types.Panel):
    bl_label = "Ornisculpt"
    bl_space_type = "PROPERTIES"
    bl_region_type = "WINDOW"
    bl_context = "object"

    def draw(self, context):
        layout = self.layout
        obj = context.active_object
        d = data(obj)
        if "part" in d:
            part = d["part"]
            kind = part.get("kind")
            kind = next(iter(kind)) if isinstance(kind, dict) else kind
            layout.label(text=f"Part {part_path(obj)}: {kind}", icon="BONE_DATA")
            attach = part.get("attach", "End")
            layout.label(text=f"Attach: {next(iter(attach)) if isinstance(attach, dict) else attach}")
        elif "garment" in d:
            g = d["garment"]
            layout.label(text=f"Garment on {g.get('on', '(where it lies)')}", icon="MOD_CLOTH")
        else:
            layout.label(text="Not part of the body")
        layout.operator_menu_enum(ORNISCULPT_OT_add_part.bl_idname, "template", text="Add Part")
        layout.operator_menu_enum(ORNISCULPT_OT_mark_part.bl_idname, "kind", text="Mark as Part")
        layout.operator(ORNISCULPT_OT_mark_garment.bl_idname)
        layout.label(text="Edit the values under Custom Properties > ornisculpt")


class ORNISCULPT_PT_scene(bpy.types.Panel):
    bl_label = "Ornisculpt"
    bl_space_type = "PROPERTIES"
    bl_region_type = "WINDOW"
    bl_context = "scene"

    def draw(self, context):
        layout = self.layout
        layout.prop(context.scene, "ornisculpt_path")
        layout.prop(context.scene, "ornisculpt_live")
        layout.operator(ORNISCULPT_OT_export.bl_idname)


CLASSES = (
    ORNISCULPT_OT_add_part,
    ORNISCULPT_OT_mark_part,
    ORNISCULPT_OT_mark_garment,
    ORNISCULPT_OT_export,
    ORNISCULPT_PT_object,
    ORNISCULPT_PT_scene,
)


def register():
    bpy.types.Scene.ornisculpt_path = bpy.props.StringProperty(
        name="File", subtype="FILE_PATH", description="The .body.glb to export to (under the game's assets/)"
    )
    bpy.types.Scene.ornisculpt_live = bpy.props.BoolProperty(
        name="Export on Save", description="Export to the file every time the .blend is saved, so a running game hot-reloads it"
    )
    for c in CLASSES:
        bpy.utils.register_class(c)
    bpy.app.handlers.save_post.append(export_on_save)


def unregister():
    if export_on_save in bpy.app.handlers.save_post:
        bpy.app.handlers.save_post.remove(export_on_save)
    for c in reversed(CLASSES):
        bpy.utils.unregister_class(c)
    del bpy.types.Scene.ornisculpt_live
    del bpy.types.Scene.ornisculpt_path
