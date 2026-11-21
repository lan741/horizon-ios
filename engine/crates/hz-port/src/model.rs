//! Model compilation: resolve `parent` chains and bake elements into quads.
//!
//! ```text
//! "MD2"
//! u16 texture_count | (str16 key, str16 resolved_texture)*
//! u16 quad_count    | per quad:
//!     u8 face (0 up, 1 down, 2 north, 3 south, 4 west, 5 east)
//!     u8 cullface (0 none, else face+1)
//!     i16 tint (-1 none)
//!     u8 rotation_degrees
//!     u16 texture_slot   index into the model's resolved texture list
//!     4 × (f32 x, f32 y, f32 z)   block-local, 0..1
//!     4 × (f32 u, f32 v)
//! ```
//! The renderer maps `texture_slot` through `Registry::texture_ids` to a slice
//! of the `texture2d_array` it uploads, so a quad never has to name a file.
//! Element rotation around `rotation.origin` is baked in. `display` transforms
//! are dropped: the engine owns item-hold and GUI transforms (documented in
//! docs/PORT_PIPELINE.md, "what is deliberately not ported").

use hz_core::{Buf, Json};

const MAX_DEPTH: usize = 16;

struct Merged {
    textures: Vec<(String, String)>,
    elements: Vec<Json>,
}

fn lookup<'a>(all: &'a [(String, Vec<u8>)], id: &str) -> Option<&'a [u8]> {
    all.iter()
        .find(|(k, _)| k == id)
        .map(|(_, v)| v.as_slice())
        .or_else(|| {
            let alt = if id.contains(':') {
                id.to_string()
            } else {
                format!("minecraft:{}", id)
            };
            all.iter()
                .find(|(k, _)| *k == alt)
                .map(|(_, v)| v.as_slice())
        })
}

fn merge(
    id: &str,
    own: Option<&Json>,
    all: &[(String, Vec<u8>)],
    depth: usize,
) -> Result<Merged, String> {
    if depth > MAX_DEPTH {
        return Err("model parent chain deeper than 16".to_string());
    }
    let j: Json = match own {
        Some(j) => j.clone(),
        None => {
            let bytes = lookup(all, id).ok_or_else(|| format!("missing model {}", id))?;
            Json::parse(&String::from_utf8_lossy(bytes))?
        }
    };

    let mut merged = match j.get("parent").and_then(|p| p.as_str()) {
        Some(parent) => merge(parent, None, all, depth + 1)?,
        None => Merged {
            textures: Vec::new(),
            elements: Vec::new(),
        },
    };

    if let Some(t) = j.get("textures").and_then(|t| t.as_obj()) {
        for (k, v) in t {
            if let Some(s) = v.as_str() {
                match merged.textures.iter_mut().find(|(mk, _)| mk == k) {
                    Some((_, mv)) => *mv = s.to_string(),
                    None => merged.textures.push((k.clone(), s.to_string())),
                }
            }
        }
    }
    if let Some(e) = j.get("elements").and_then(|e| e.as_arr()) {
        merged.elements = e.to_vec();
    }
    Ok(merged)
}

#[derive(Clone, Copy)]
enum Face {
    Up = 0,
    Down = 1,
    North = 2,
    South = 3,
    West = 4,
    East = 5,
}

fn face_from_name(n: &str) -> Option<Face> {
    match n {
        "up" => Some(Face::Up),
        "down" => Some(Face::Down),
        "north" => Some(Face::North),
        "south" => Some(Face::South),
        "west" => Some(Face::West),
        "east" => Some(Face::East),
        _ => None,
    }
}

fn nums(v: &Json, n: usize) -> Option<Vec<f32>> {
    let a = v.as_arr()?;
    if a.len() < n {
        return None;
    }
    Some(
        a.iter()
            .take(n)
            .filter_map(|x| x.as_f64())
            .map(|x| x as f32)
            .collect(),
    )
}

/// Quad corners for a box face, in Minecraft's 0..16 space.
fn quad_for(face: Face, from: [f32; 3], to: [f32; 3]) -> [[f32; 3]; 4] {
    let (x0, y0, z0) = (from[0], from[1], from[2]);
    let (x1, y1, z1) = (to[0], to[1], to[2]);
    match face {
        Face::North => [[x1, y0, z0], [x0, y0, z0], [x0, y1, z0], [x1, y1, z0]],
        Face::South => [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]],
        Face::West => [[x0, y0, z1], [x0, y0, z0], [x0, y1, z0], [x0, y1, z1]],
        Face::East => [[x1, y0, z0], [x1, y0, z1], [x1, y1, z1], [x1, y1, z0]],
        Face::Up => [[x0, y1, z1], [x1, y1, z1], [x1, y1, z0], [x0, y1, z0]],
        Face::Down => [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
    }
}

fn rotate_point(p: [f32; 3], origin: [f32; 3], axis: &str, deg: f32) -> [f32; 3] {
    if deg == 0.0 {
        return p;
    }
    let a = deg.to_radians();
    let (s, c) = (a.sin(), a.cos());
    let mut d = [p[0] - origin[0], p[1] - origin[1], p[2] - origin[2]];
    d = match axis {
        "x" => [d[0], d[1] * c - d[2] * s, d[1] * s + d[2] * c],
        "y" => [d[0] * c + d[2] * s, d[1], -d[0] * s + d[2] * c],
        "z" => [d[0] * c - d[1] * s, d[0] * s + d[1] * c, d[2]],
        _ => d,
    };
    [d[0] + origin[0], d[1] + origin[1], d[2] + origin[2]]
}

fn resolve_texture(raw: &str, textures: &[(String, String)]) -> String {
    if let Some(key) = raw.strip_prefix('#') {
        if let Some((_, v)) = textures.iter().find(|(k, _)| k == key) {
            return resolve_texture(v, textures);
        }
        return format!("missing:{}", key);
    }
    if raw.contains(':') {
        raw.to_string()
    } else {
        format!("minecraft:{}", raw)
    }
}

pub fn compile(id: &str, j: &Json, all: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let merged = merge(id, Some(j), all, 0)?;

    let mut tex_pairs: Vec<(String, String)> = merged
        .textures
        .iter()
        .map(|(k, v)| (k.clone(), resolve_texture(v, &merged.textures)))
        .collect();
    tex_pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut quads: Vec<([f32; 3], [f32; 3], [f32; 3], [f32; 3])> = Vec::new();
    #[allow(clippy::type_complexity)]
    let mut meta: Vec<(
        u8,
        u8,
        i16,
        f32,
        u16,
        [f32; 2],
        [f32; 2],
        [f32; 2],
        [f32; 2],
    )> = Vec::new();

    for el in &merged.elements {
        let from = el.get("from").and_then(|v| nums(v, 3));
        let to = el.get("to").and_then(|v| nums(v, 3));
        let (from, to) = match (from, to) {
            (Some(f), Some(t)) => ([f[0], f[1], f[2]], [t[0], t[1], t[2]]),
            _ => continue,
        };
        let (origin, axis, angle) = match el.get("rotation") {
            Some(r) => {
                let o = r
                    .get("origin")
                    .and_then(|v| nums(v, 3))
                    .unwrap_or(vec![8.0, 8.0, 8.0]);
                (
                    [o[0], o[1], o[2]],
                    r.get("axis").and_then(|v| v.as_str()).unwrap_or("y"),
                    r.get("angle").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                )
            }
            None => ([8.0, 8.0, 8.0], "y", 0.0),
        };
        let faces = match el.get("faces").and_then(|f| f.as_obj()) {
            Some(f) => f,
            None => continue,
        };
        for (fname, fdesc) in faces {
            let face = match face_from_name(fname) {
                Some(f) => f,
                None => continue,
            };
            let texture = fdesc
                .get("texture")
                .and_then(|t| t.as_str())
                .unwrap_or("missing");
            let resolved = resolve_texture(texture, &merged.textures);
            let slot = tex_pairs
                .iter()
                .position(|(_, v)| *v == resolved)
                .unwrap_or(usize::MAX);
            let uv = fdesc
                .get("uv")
                .and_then(|v| nums(v, 4))
                .unwrap_or(vec![0.0, 0.0, 16.0, 16.0]);
            let cull = fdesc
                .get("cullface")
                .and_then(|c| c.as_str())
                .and_then(face_from_name)
                .map(|f| f as u8 + 1)
                .unwrap_or(0);
            let rot = fdesc
                .get("rotation")
                .and_then(|r| r.as_f64())
                .unwrap_or(0.0) as f32;
            let tint = fdesc
                .get("tintindex")
                .and_then(|t| t.as_f64())
                .map(|t| t as i16)
                .unwrap_or(-1);
            let mut corners = quad_for(face, from, to);
            if angle != 0.0 {
                for c in corners.iter_mut() {
                    *c = rotate_point(*c, origin, axis, angle);
                }
            }
            for c in corners.iter_mut() {
                c[0] = (c[0] / 16.0).clamp(0.0, 1.0);
                c[1] = (c[1] / 16.0).clamp(0.0, 1.0);
                c[2] = (c[2] / 16.0).clamp(0.0, 1.0);
            }
            quads.push((corners[0], corners[1], corners[2], corners[3]));
            meta.push((
                face as u8,
                cull,
                tint,
                rot,
                if slot == usize::MAX {
                    u16::MAX
                } else {
                    slot as u16
                },
                [uv[0] / 16.0, uv[1] / 16.0],
                [uv[2] / 16.0, uv[1] / 16.0],
                [uv[2] / 16.0, uv[3] / 16.0],
                [uv[0] / 16.0, uv[3] / 16.0],
            ));
        }
    }

    let mut buf = Buf::new();
    buf.bytes(b"MD2");
    buf.u16(tex_pairs.len() as u16);
    for (k, v) in &tex_pairs {
        buf.str16(k);
        buf.str16(v);
    }
    buf.u16(quads.len() as u16);
    for (i, q) in quads.iter().enumerate() {
        let (face, cull, tint, rot, slot, uv0, uv1, uv2, uv3) = meta[i];
        buf.u8(face);
        buf.u8(cull);
        buf.i16(tint);
        buf.u8(rot.clamp(0.0, 255.0) as u8);
        buf.u16(slot);
        for c in [q.0, q.1, q.2, q.3] {
            buf.f32(c[0]);
            buf.f32(c[1]);
            buf.f32(c[2]);
        }
        buf.f32(uv0[0]);
        buf.f32(uv0[1]);
        buf.f32(uv1[0]);
        buf.f32(uv1[1]);
        buf.f32(uv2[0]);
        buf.f32(uv2[1]);
        buf.f32(uv3[0]);
        buf.f32(uv3[1]);
    }
    Ok(buf.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_chain_and_quads() {
        let parent =
            br##"{"textures":{"all":"minecraft:block/stone","particle":"#all"},"elements":[
            {"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all"}}}]}"##
                .to_vec();
        let all = vec![
            ("minecraft:block/cube_all".to_string(), parent),
            (
                "minecraft:block/stone".to_string(),
                br#"{"parent":"minecraft:block/cube_all"}"#.to_vec(),
            ),
        ];
        let own = Json::parse(r#"{"parent":"minecraft:block/cube_all"}"#).unwrap();
        let bin = compile("minecraft:block/stone", &own, &all).unwrap();
        let mut r = hz_core::util::Reader::new(&bin);
        assert_eq!(r.take(3).unwrap(), b"MD2");
        assert_eq!(r.u16().unwrap(), 2); // all + particle
        for _ in 0..2 {
            r.str16().unwrap();
            r.str16().unwrap();
        }
        assert_eq!(r.u16().unwrap(), 1); // one quad
        assert_eq!(r.u8().unwrap(), 0); // up
        r.u8().unwrap(); // cullface
        r.take(2).unwrap(); // tint
        r.u8().unwrap(); // rotation
        let slot = r.u16().unwrap();
        assert!(slot < 2, "texture slot {slot} outside the model's table");
    }

    #[test]
    fn rotate_wraps() {
        let p = rotate_point([16.0, 0.0, 8.0], [8.0, 8.0, 8.0], "y", 90.0);
        assert!((p[0] - 8.0).abs() < 1e-4);
        assert!((p[2] - 0.0).abs() < 1e-4);
    }
}
