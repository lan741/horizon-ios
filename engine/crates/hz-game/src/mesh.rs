//! Section meshing: the cached, incremental half of the renderer.
//!
//! A `SectionMesh` is the baked vertex data for one 16³ section. It is rebuilt
//! only when something inside it (or on its boundary) changed; `Game` keeps the
//! cache and concatenates the visible meshes into the frame buffer.
//!
//! This is what turns "walk 400 k blocks every tick" into "walk the sections
//! that actually changed". Idle frames cost nothing at all.

use std::collections::HashMap;

use crate::chunk::Chunk;
use crate::frame::{Frame, FLOATS_PER_VERTEX};
use crate::light::{ao_factor, light_curve, vertex_ao, LightField};
use crate::registry::Registry;
use crate::SECTION;

/// Faces in Minecraft's order; the cull vector for a face is the direction a
/// neighbour must be in for that face to be hidden.
pub fn face_normal(face: u8) -> [i8; 3] {
    match face {
        0 => [0, 1, 0],
        1 => [0, -1, 0],
        2 => [0, 0, -1],
        3 => [0, 0, 1],
        4 => [-1, 0, 0],
        5 => [1, 0, 0],
        _ => [0, 1, 0],
    }
}

/// Baked vertices for one section, already in world space.
#[derive(Default, Clone)]
pub struct SectionMesh {
    pub verts: Vec<f32>,
    pub quads: u32,
    /// Texture slots with more than one frame that this mesh sampled, so a
    /// frame advance invalidates exactly the sections it affects.
    pub animated: Vec<u16>,
    /// Set when the block data or an animation frame changed.
    pub dirty: bool,
    /// Bumped every time the mesh is re-baked. The renderer keeps its own copy
    /// per section and only re-uploads the ones whose generation moved.
    pub generation: u32,
}

impl SectionMesh {
    pub fn vertex_count(&self) -> u32 {
        (self.verts.len() / FLOATS_PER_VERTEX) as u32
    }

    pub fn clear(&mut self) {
        self.verts.clear();
        self.quads = 0;
        self.animated.clear();
    }

    /// Bytes the renderer has to upload for this section.
    pub fn byte_len(&self) -> usize {
        self.verts.len() * std::mem::size_of::<f32>()
    }
}

/// Everything needed to bake one section. Kept separate from `Game` so the
/// bake function has no borrow of the whole game.
pub struct BakeInput<'a> {
    pub key: (i32, i32, i32),
    pub chunk: &'a Chunk,
    pub registry: &'a Registry,
    pub air: u16,
    /// Tick used to pick the animation frame baked into the UVs.
    pub tick: u64,
    /// Block lookup that can cross section boundaries.
    pub neighbour: &'a dyn Fn(i32, i32, i32) -> u16,
    /// Lighting for this world. `None` bakes unlit geometry, which the geometry
    /// tests use; the game always supplies one.
    pub light: Option<&'a LightField>,
    /// Daylight multiplier applied to the sky channel (0 = night, 1 = noon).
    pub daylight: f32,
    /// Whether a block stops light, for ambient occlusion lookups.
    pub opaque: &'a dyn Fn(i32, i32, i32) -> bool,
}

/// Bake one section into `out`. `out` is cleared first and reused, so a busy
/// world does not reallocate per edit.
pub fn bake_section(input: &BakeInput<'_>, out: &mut SectionMesh) {
    out.clear();
    let mut animated: Vec<u16> = Vec::new();
    let (sx, sy, sz) = input.key;
    let cx = sx * SECTION;
    let cy = sy * SECTION;
    let cz = sz * SECTION;

    for y in 0..SECTION as usize {
        for z in 0..SECTION as usize {
            for x in 0..SECTION as usize {
                let id = input.chunk.get(x, y, z);
                if id == input.air {
                    continue;
                }
                let model = match input.registry.model_for(id, 0) {
                    Some(m) if !m.quads.is_empty() => m,
                    _ => continue,
                };
                let wx = cx + x as i32;
                let wy = cy + y as i32;
                let wz = cz + z as i32;
                let origin = [wx as f32, wy as f32, wz as f32];

                for q in &model.quads {
                    // Animated textures are tall strips: pick this tick's frame
                    // by squeezing V into it.
                    let mut uv = q.uv;
                    let mut v_delta = 0.0f32;
                    let mut blend = 0.0f32;
                    if let Some(anim) = input.registry.animations.get(&q.texture) {
                        let pair = anim.frame_pair(input.tick);
                        let scale = anim.v_scale();
                        let offset = pair.current as f32 * scale;
                        for corner in uv.iter_mut() {
                            corner[1] = corner[1] * scale + offset;
                        }
                        if anim.interpolate && anim.frame_count() > 1 {
                            v_delta = anim.v_delta(pair);
                            blend = pair.blend;
                        }
                        if !animated.contains(&q.texture) {
                            animated.push(q.texture);
                        }
                    }
                    if q.cullface != 0 {
                        let n = face_normal(q.cullface - 1);
                        let nb =
                            (input.neighbour)(wx + n[0] as i32, wy + n[1] as i32, wz + n[2] as i32);
                        if nb != input.air {
                            continue;
                        }
                    }
                    let mut corners = [[0.0f32; 3]; 4];
                    for (ci, c) in q.pos.iter().enumerate() {
                        corners[ci] = [origin[0] + c[0], origin[1] + c[1], origin[2] + c[2]];
                    }
                    let n = face_normal(q.face);
                    let colors = face_colors(input, wx, wy, wz, n, q.face, &q.pos);
                    push_quad(
                        &mut out.verts,
                        &corners,
                        &uv,
                        [n[0] as f32, n[1] as f32, n[2] as f32],
                        &colors,
                        q.texture,
                        v_delta,
                        blend,
                    );
                    out.quads += 1;
                }
            }
        }
    }
    out.animated = animated;
    out.dirty = false;
}

/// Same vertex layout as `Frame::push_quad`, written straight into a section
/// buffer so sections never need an intermediate allocation.
#[allow(clippy::too_many_arguments)]
fn push_quad(
    out: &mut Vec<f32>,
    corners: &[[f32; 3]; 4],
    uv: &[[f32; 2]; 4],
    normal: [f32; 3],
    colors: &[[f32; 4]; 4],
    texture: u16,
    v_delta: f32,
    blend: f32,
) {
    let t = if texture == u16::MAX {
        f32::from(u16::MAX)
    } else {
        f32::from(texture)
    };
    for i in [0usize, 1, 2, 2, 3, 0] {
        let p = corners[i];
        let uv = uv[i];
        let c = colors[i];
        out.extend_from_slice(&[
            p[0], p[1], p[2], uv[0], uv[1], normal[0], normal[1], normal[2], c[0], c[1], c[2],
            c[3], t, v_delta, blend, 0.0,
        ]);
    }
}

/// Per-corner brightness: face light (sampled in front of the face) times the
/// corner's ambient occlusion.
///
/// AO uses the three neighbours in the face plane: two edges and the diagonal.
/// The tangent signs come from the corner's own block-local position, which is
/// exact for these axis-aligned quads and avoids hard-coding a corner order.
fn face_colors(
    input: &BakeInput<'_>,
    wx: i32,
    wy: i32,
    wz: i32,
    normal: [i8; 3],
    _face: u8,
    local: &[[f32; 3]; 4],
) -> [[f32; 4]; 4] {
    let light = match input.light {
        Some(l) => l,
        None => return [[1.0, 1.0, 1.0, 1.0]; 4],
    };
    // Light is sampled in the empty cell in front of the face, like vanilla.
    let front = (
        wx + normal[0] as i32,
        wy + normal[1] as i32,
        wz + normal[2] as i32,
    );
    let level = light.brightness(front.0, front.1, front.2, input.daylight);
    let base = light_curve(level);

    // Tangent axes are the two the normal is not on.
    let axis = if normal[0] != 0 {
        0
    } else if normal[1] != 0 {
        1
    } else {
        2
    };
    let (b, c) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };

    let mut out = [[0.0f32; 4]; 4];
    for (i, corner) in local.iter().enumerate() {
        let sb = if corner[b] < 0.5 { -1 } else { 1 };
        let sc = if corner[c] < 0.5 { -1 } else { 1 };
        let mut nb = [0i32; 3];
        nb[b] = sb;
        let mut nc = [0i32; 3];
        nc[c] = sc;
        let side1 = (input.opaque)(front.0 + nb[0], front.1 + nb[1], front.2 + nb[2]);
        let side2 = (input.opaque)(front.0 + nc[0], front.1 + nc[1], front.2 + nc[2]);
        let corner_block = (input.opaque)(
            front.0 + nb[0] + nc[0],
            front.1 + nb[1] + nc[1],
            front.2 + nb[2] + nc[2],
        );
        let ao = vertex_ao(side1, side2, corner_block);
        let f = base * ao_factor(ao);
        out[i] = [f, f, f, 1.0];
    }
    out
}

/// Cache of section meshes plus the bookkeeping needed to know when the frame
/// buffer has to be reassembled.
pub struct MeshCache {
    meshes: HashMap<(i32, i32, i32), SectionMesh>,
    /// View parameters at the last assembly, so moving across a block boundary
    /// invalidates the visible set.
    last_view: Option<ViewKey>,
    /// Set by any block edit or view change.
    needs_assembly: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ViewKey {
    pub block: (i32, i32, i32),
    pub radius: i32,
}

impl Default for MeshCache {
    fn default() -> Self {
        MeshCache {
            meshes: HashMap::new(),
            last_view: None,
            needs_assembly: true,
        }
    }
}

impl MeshCache {
    pub fn mark_dirty(&mut self, key: (i32, i32, i32)) {
        if let Some(mesh) = self.meshes.get_mut(&key) {
            mesh.dirty = true;
        }
        self.needs_assembly = true;
    }

    /// Mark every section that sampled `slot` dirty; used when its animation
    /// frame advances.
    pub fn mark_texture_animated(&mut self, slot: u16) -> usize {
        let mut touched = 0;
        for mesh in self.meshes.values_mut() {
            if mesh.animated.contains(&slot) {
                mesh.dirty = true;
                touched += 1;
            }
        }
        if touched > 0 {
            self.needs_assembly = true;
        }
        touched
    }

    pub fn invalidate_all(&mut self) {
        for mesh in self.meshes.values_mut() {
            mesh.dirty = true;
        }
        self.needs_assembly = true;
    }

    pub fn forget(&mut self, key: (i32, i32, i32)) {
        self.meshes.remove(&key);
        self.needs_assembly = true;
    }

    pub fn get(&self, key: (i32, i32, i32)) -> Option<&SectionMesh> {
        self.meshes.get(&key)
    }

    pub fn entry(&mut self, key: (i32, i32, i32)) -> &mut SectionMesh {
        self.meshes.entry(key).or_default()
    }

    pub fn len(&self) -> usize {
        self.meshes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.meshes.is_empty()
    }

    /// True when `assemble` would actually do work.
    pub fn needs_work(&self, view: ViewKey) -> bool {
        self.needs_assembly || self.last_view != Some(view)
    }

    /// Record that a caller consumed the current state without assembling the
    /// flat frame: a renderer with per-section buffers does exactly that.
    pub fn mark_assembled(&mut self, view: ViewKey) {
        self.last_view = Some(view);
        self.needs_assembly = false;
    }

    /// Sections that should be drawn, in deterministic order.
    ///
    /// The renderer uses this to decide which buffers to draw and which to
    /// re-upload (by generation); `assemble` uses it to build the flat frame the
    /// benchmarks and the fallback path need.
    pub fn visible(&self, view: ViewKey) -> Vec<((i32, i32, i32), u32, u32)> {
        let r = view.radius * SECTION;
        let (px, py, pz) = view.block;
        let mut out: Vec<((i32, i32, i32), u32, u32)> = self
            .meshes
            .iter()
            .filter(|(_, mesh)| !mesh.verts.is_empty())
            .filter(|((sx, sy, sz), _)| {
                let cx = sx * SECTION;
                let cy = sy * SECTION;
                let cz = sz * SECTION;
                // section box vs. the cube around the player
                cx + SECTION >= px - r
                    && cx <= px + r
                    && cy + SECTION >= py - r
                    && cy <= py + r
                    && cz + SECTION >= pz - r
                    && cz <= pz + r
            })
            .map(|(key, mesh)| (*key, mesh.vertex_count(), mesh.generation))
            .collect();
        out.sort_unstable_by_key(|(key, _, _)| *key);
        out
    }

    /// Vertices of one section, or `None` when it is not cached.
    pub fn section_vertices(&self, key: (i32, i32, i32)) -> Option<&[f32]> {
        self.meshes.get(&key).map(|m| m.verts.as_slice())
    }

    /// Concatenate every visible section's mesh into `frame`. Sections are
    /// visited in sorted order so two runs over the same world produce
    /// byte-identical frames.
    pub fn assemble(&mut self, view: ViewKey, frame: &mut Frame) {
        frame.clear();
        for (key, _, _) in self.visible(view) {
            if let Some(mesh) = self.meshes.get(&key) {
                frame.verts.extend_from_slice(&mesh.verts);
                frame.quads += mesh.quads;
            }
        }
        self.last_view = Some(view);
        self.needs_assembly = false;
    }

    /// Total bytes the flat assembler would copy for this view — the number the
    /// per-section path avoids re-copying.
    pub fn visible_byte_len(&self, view: ViewKey) -> usize {
        self.visible(view)
            .iter()
            .map(|(key, _, _)| self.meshes.get(key).map(|m| m.byte_len()).unwrap_or(0))
            .sum()
    }
}
