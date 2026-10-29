//! Triangle meshes out of glTF files, without the asset server: `.glb`, or `.gltf` with its
//! buffers embedded as base64 data URIs (as `tools/` write them).

use bevy::prelude::*;
use serde_json::Value;

use super::read_glb;
use crate::anatomy::TriMesh;

/// The glTF document in `bytes` (`.glb` or `.gltf` text) and its buffers' bytes.
#[must_use]
pub fn gltf_document(bytes: &[u8]) -> Option<(Value, Vec<Vec<u8>>)> {
    use base64::Engine;
    let (doc, bin) = if bytes.starts_with(b"glTF") {
        let (doc, bin) = read_glb(bytes).ok()?;
        (doc, Some(bin.to_vec()))
    } else {
        (serde_json::from_slice(bytes).ok()?, None)
    };
    let mut bin = bin;
    // Only data URIs: a file next to this one would need the asset server.
    let data_uri = |uri: &str| {
        uri.split_once(";base64,")
            .and_then(|(_, data)| base64::engine::general_purpose::STANDARD.decode(data).ok())
            .unwrap_or_default()
    };
    let buffers = doc.get("buffers").and_then(Value::as_array).map_or_default(|buffers| {
        buffers
            .iter()
            .map(|b| b.get("uri").and_then(Value::as_str).map_or_else(|| bin.take().unwrap_or_default(), data_uri))
            .collect()
    });
    Some((doc, buffers))
}

/// Mesh primitive `label` (`Mesh0/Primitive0`; the first when empty) of the glTF in `bytes`:
/// positions, normals (empty when it has none) and triangle indices.
#[must_use]
pub fn read_trimesh(bytes: &[u8], label: &str) -> Option<TriMesh> {
    let (doc, buffers) = gltf_document(bytes)?;
    let number = |prefix: &str| label.split('/').find_map(|l| l.strip_prefix(prefix)?.parse::<usize>().ok()).unwrap_or(0);
    let prim = doc.pointer(&format!("/meshes/{}/primitives/{}", number("Mesh"), number("Primitive")))?;
    if prim.get("mode").and_then(Value::as_u64).is_some_and(|m| m != 4) {
        return None; // not a triangle list
    }
    trimesh(&doc, &buffers, prim)
}

/// The triangles of one primitive (`prim`, a JSON object) of `doc`.
#[must_use]
pub fn trimesh(doc: &Value, buffers: &[Vec<u8>], prim: &Value) -> Option<TriMesh> {
    let attr = |name: &str| prim.pointer(&format!("/attributes/{name}")).and_then(Value::as_u64);
    let positions = vec3s(doc, buffers, attr("POSITION")?)?;
    let normals = attr("NORMAL").and_then(|a| vec3s(doc, buffers, a)).unwrap_or_default();
    let indices = match prim.get("indices").and_then(Value::as_u64) {
        Some(a) => scalars(doc, buffers, a)?,
        None => (0..u32::try_from(positions.len()).ok()?).collect(),
    };
    Some(TriMesh::new(positions, normals, indices))
}

/// Accessor `index`'s bytes per element, the element count, component type and its data
/// (strided views are read element by element).
fn accessor<'a>(doc: &Value, buffers: &'a [Vec<u8>], index: u64) -> Option<(u64, usize, impl Iterator<Item = &'a [u8]>)> {
    let acc = doc.pointer(&format!("/accessors/{index}"))?;
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok());
    let view = doc.pointer(&format!("/bufferViews/{}", acc.get("bufferView")?.as_u64()?))?;
    let buffer = buffers.get(num(view, "buffer").unwrap_or(0))?;
    let component = acc.get("componentType")?.as_u64()?;
    let count = num(acc, "count")?;
    let width = match acc.get("type")?.as_str()? {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        _ => return None,
    };
    let size = width * match component {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        _ => 4,
    };
    let start = num(view, "byteOffset").unwrap_or(0) + num(acc, "byteOffset").unwrap_or(0);
    let stride = num(view, "byteStride").unwrap_or(size).max(size);
    let end = start + stride * count.saturating_sub(1) + size;
    let data = buffer.get(start..end)?;
    Some((component, count, (0..count).filter_map(move |i| data.get(i * stride..i * stride + size))))
}

fn vec3s(doc: &Value, buffers: &[Vec<u8>], index: u64) -> Option<Vec<Vec3>> {
    let (component, count, elements) = accessor(doc, buffers, index)?;
    if component != 5126 {
        return None;
    }
    let f = |b: &[u8], i: usize| b.get(i * 4..i * 4 + 4).and_then(|w| <[u8; 4]>::try_from(w).ok()).map_or(0.0, f32::from_le_bytes);
    let out: Vec<Vec3> = elements.map(|e| Vec3::new(f(e, 0), f(e, 1), f(e, 2))).collect();
    (out.len() == count).then_some(out)
}

fn scalars(doc: &Value, buffers: &[Vec<u8>], index: u64) -> Option<Vec<u32>> {
    let (component, count, elements) = accessor(doc, buffers, index)?;
    let out: Vec<u32> = elements
        .filter_map(|e| match component {
            5121 => e.first().copied().map(u32::from),
            5123 => <[u8; 2]>::try_from(e).ok().map(|b| u32::from(u16::from_le_bytes(b))),
            5125 => <[u8; 4]>::try_from(e).ok().map(u32::from_le_bytes),
            _ => None,
        })
        .collect();
    (out.len() == count).then_some(out)
}
