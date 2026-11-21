//! The game loop: world storage, a player capsule, and the visible-frame build.

use std::collections::HashMap;

use hz_core::Pack;

use crate::chunk::Chunk;
use crate::frame::Frame;
use crate::light::{compute, Bounds, HeightMap, LightField};
use crate::mesh::{bake_section, BakeInput, MeshCache, ViewKey};
use crate::registry::Registry;
use crate::SECTION;

pub struct World {
    sections: HashMap<(i32, i32, i32), Chunk>,
    pub air: u16,
}

impl World {
    pub fn new(air: u16) -> World {
        World {
            sections: HashMap::new(),
            air,
        }
    }

    fn key(x: i32, y: i32, z: i32) -> (i32, i32, i32) {
        (
            x.div_euclid(SECTION),
            y.div_euclid(SECTION),
            z.div_euclid(SECTION),
        )
    }

    pub fn get(&self, x: i32, y: i32, z: i32) -> u16 {
        let k = Self::key(x, y, z);
        match self.sections.get(&k) {
            None => self.air,
            Some(c) => c.get(
                x.rem_euclid(SECTION) as usize,
                y.rem_euclid(SECTION) as usize,
                z.rem_euclid(SECTION) as usize,
            ),
        }
    }

    pub fn set(&mut self, x: i32, y: i32, z: i32, id: u16) {
        let k = Self::key(x, y, z);
        let c = self
            .sections
            .entry(k)
            .or_insert_with(|| Chunk::new(self.air));
        c.set(
            x.rem_euclid(SECTION) as usize,
            y.rem_euclid(SECTION) as usize,
            z.rem_euclid(SECTION) as usize,
            id,
        );
    }

    pub fn sections(&self) -> impl Iterator<Item = (&(i32, i32, i32), &Chunk)> {
        self.sections.iter()
    }

    pub fn insert(&mut self, key: (i32, i32, i32), chunk: Chunk) {
        self.sections.insert(key, chunk);
    }

    pub fn clear(&mut self) {
        self.sections.clear();
    }

    /// Lowest loaded section floor; used when a column loses its top block.
    pub fn min_y(&self) -> i32 {
        self.sections
            .keys()
            .map(|(_, sy, _)| sy * SECTION)
            .min()
            .unwrap_or(0)
    }

    pub fn non_air(&self) -> u32 {
        self.sections.values().map(|c| c.non_air).sum()
    }

    /// A flat 32×32 stone pad at y=0, so the app can show something before a
    /// real world is imported (and so tests have geometry).
    pub fn flat_pad(&mut self, stone: u16, half: i32) {
        for x in -half..=half {
            for z in -half..=half {
                self.set(x, 0, z, stone);
                for y in 1..=3 {
                    self.set(x, y, z, self.air);
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Player {
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub flying: bool,
}

impl Default for Player {
    fn default() -> Self {
        Player {
            pos: [0.5, 2.0, 0.5],
            vel: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
            flying: false,
        }
    }
}

/// What a ray hit: the block, the face it entered through (unit step, so it is
/// also the offset where a placed block goes), and how far away it was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    pub block: (i32, i32, i32),
    pub normal: (i8, i8, i8),
    pub distance: f32,
}

impl RayHit {
    /// Where a "place" action should put the new block.
    pub fn adjacent(&self) -> (i32, i32, i32) {
        (
            self.block.0 + self.normal.0 as i32,
            self.block.1 + self.normal.1 as i32,
            self.block.2 + self.normal.2 as i32,
        )
    }
}

const GRAVITY: f32 = -28.0;
const WALK: f32 = 4.3;
const HALF_W: f32 = 0.3;
const HEIGHT: f32 = 1.8;

pub struct Game {
    pub registry: Registry,
    pub world: World,
    pub player: Player,
    frame: Frame,
    /// Per-section baked vertices; the reason idle frames are free.
    meshes: MeshCache,
    /// Last frame index baked for each animated texture slot.
    anim_frames: std::collections::HashMap<u16, u16>,
    /// Highest opaque block per column, maintained on every edit.
    heights: HeightMap,
    /// Both light channels over the loaded sections.
    light: Option<LightField>,
    /// Monotonic counter handed to sections when they are re-baked.
    next_generation: u32,
    /// Bumped whenever the visible section set or any vertices change. The
    /// renderer compares it to decide whether a frame needs any work at all.
    revision: u32,
    /// Edits whose lighting still has to be recomputed.
    light_edits: Vec<(i32, i32, i32)>,
    /// Set when the whole field has to be rebuilt (load, bulk fill, too many
    /// edits in one frame).
    light_dirty: bool,
    /// Daylight multiplier for the sky channel (0 night … 1 noon).
    daylight: f32,
    /// Sounds waiting for the platform to play them.
    sounds: crate::sound::SoundQueue,
    pub ticks: u64,
    pub frames: u64,
    pub draw_radius: i32,
}

impl Game {
    /// `pack_bytes` is a `.hzp` file, mapped or copied by the caller.
    pub fn new(pack_bytes: &[u8]) -> Result<Game, String> {
        let pack = Pack::read(pack_bytes)?;
        let registry = Registry::build(&pack)?;
        let air = registry.air;
        let stone = registry.stone;
        let mut world = World::new(air);
        world.flat_pad(stone, 8);
        let mut game = Game {
            registry,
            world,
            player: Player::default(),
            frame: Frame::new(),
            meshes: MeshCache::default(),
            anim_frames: std::collections::HashMap::new(),
            heights: HeightMap::default(),
            light: None,
            next_generation: 1,
            revision: 1,
            light_edits: Vec::new(),
            light_dirty: true,
            daylight: 1.0,
            sounds: crate::sound::SoundQueue::default(),
            ticks: 0,
            frames: 0,
            draw_radius: 1,
        };
        game.recompute_heights();
        Ok(game)
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// Write a block and invalidate every section whose culling it can change.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, id: u16) {
        if self.world.get(x, y, z) == id {
            return;
        }
        self.world.set(x, y, z, id);
        self.update_height(x, y, z, id);
        // More than a handful of edits in one frame is cheaper to relight in
        // one go than window by window.
        if self.light_edits.len() >= 48 {
            self.light_dirty = true;
            self.light_edits.clear();
        } else {
            self.light_edits.push((x, y, z));
        }
        self.invalidate_around(x, y, z);
    }

    /// Keep the column heightmap in step with an edit.
    fn update_height(&mut self, x: i32, y: i32, z: i32, id: u16) {
        let opaque = self.registry.is_opaque(id);
        let current = self.heights.get(x, z);
        if opaque && current.map(|h| y > h).unwrap_or(true) {
            self.heights.set(x, z, y);
            return;
        }
        if !opaque && current == Some(y) {
            // The column's top block went away: walk down to the next one.
            let floor = self.world.min_y();
            let mut next = None;
            for candidate in (floor..y).rev() {
                if self.registry.is_opaque(self.world.get(x, candidate, z)) {
                    next = Some(candidate);
                    break;
                }
            }
            match next {
                Some(h) => self.heights.set(x, z, h),
                None => self.heights.remove(x, z),
            }
        }
    }

    /// Rebuild the column heightmap from the loaded world. Needed after loading
    /// a save or after any bulk fill that bypassed `set_block`.
    pub fn recompute_heights(&mut self) {
        self.heights = HeightMap::default();
        let keys: Vec<(i32, i32, i32)> = self.world.sections().map(|(k, _)| *k).collect();
        for key in keys {
            for lx in 0..SECTION {
                for lz in 0..SECTION {
                    let x = key.0 * SECTION + lx;
                    let z = key.2 * SECTION + lz;
                    for ly in (0..SECTION).rev() {
                        let y = key.1 * SECTION + ly;
                        if self.registry.is_opaque(self.world.get(x, y, z)) {
                            let current = self.heights.get(x, z);
                            if current.map(|h| y > h).unwrap_or(true) {
                                self.heights.set(x, z, y);
                            }
                            break;
                        }
                    }
                }
            }
        }
        self.light_dirty = true;
        self.light_edits.clear();
        self.meshes.invalidate_all();
    }

    /// Recompute both light channels for the loaded sections and mark every
    /// section whose light actually changed as needing a re-bake.
    pub fn relight(&mut self) {
        let keys: Vec<(i32, i32, i32)> = self.world.sections().map(|(k, _)| *k).collect();
        let bounds = match Bounds::from_sections(&keys) {
            Some(b) => b,
            None => {
                self.light = None;
                self.light_dirty = false;
                return;
            }
        };
        let opaque = |x: i32, y: i32, z: i32| self.registry.is_opaque(self.world.get(x, y, z));
        let emission = |x: i32, y: i32, z: i32| {
            let id = self.world.get(x, y, z);
            if self.registry.is_opaque(id) {
                0
            } else {
                self.registry.light_emission(id)
            }
        };
        let field = compute(bounds, &opaque, &emission, &self.heights);

        // Only sections whose light differs need a new mesh.
        let mut changed = Vec::new();
        for key in &keys {
            let new = field.section_bytes(*key);
            let old = self.light.as_ref().and_then(|f| f.section_bytes(*key));
            if new != old {
                changed.push(*key);
            }
        }
        for key in changed {
            self.meshes.mark_dirty(key);
        }
        self.light = Some(field);
        self.light_dirty = false;
    }

    /// Recompute light only in a 16-cell window around one edit.
    ///
    /// Light fades by at least one per step, so nothing further than 16 cells
    /// can have changed; the window's boundary is seeded from the current field,
    /// which is still valid there. The interior is blitted back, then only the
    /// sections that actually differ are marked dirty.
    pub fn relight_around(&mut self, x: i32, y: i32, z: i32) {
        let current = match self.light.take() {
            Some(f) => f,
            None => {
                self.relight();
                return;
            }
        };
        // If the world grew since the field was built, the field's bounds no
        // longer cover it and a window cannot be seeded correctly. Rebuild.
        let loaded =
            Bounds::from_sections(&self.world.sections().map(|(k, _)| *k).collect::<Vec<_>>());
        if loaded != Some(current.bounds) {
            self.light = Some(current);
            self.relight();
            return;
        }
        let window = Bounds::around(x, y, z, 16, &current.bounds);
        let opaque =
            |cx: i32, cy: i32, cz: i32| self.registry.is_opaque(self.world.get(cx, cy, cz));
        let emission = |cx: i32, cy: i32, cz: i32| {
            let id = self.world.get(cx, cy, cz);
            if self.registry.is_opaque(id) {
                0
            } else {
                self.registry.light_emission(id)
            }
        };
        // A shell cell is only a valid boundary seed when that face of the window
        // is *inside* the loaded world. When the window is clipped by the world
        // edge, the "shell" sits closer to the edit than light can travel, so it
        // has to be recomputed and written back like any interior cell.
        let world_bounds = current.bounds;
        let seeded_cell = move |sx: i32, sy: i32, sz: i32| -> bool {
            let at_min = |v: i32, lo: i32, world_lo: i32| v == lo && lo > world_lo;
            let at_max = |v: i32, hi: i32, world_hi: i32| v == hi - 1 && hi - 1 < world_hi;
            at_min(sx, window.min[0], world_bounds.min[0])
                || at_min(sy, window.min[1], world_bounds.min[1])
                || at_min(sz, window.min[2], world_bounds.min[2])
                || at_max(sx, window.max[0], world_bounds.max[0])
                || at_max(sy, window.max[1], world_bounds.max[1])
                || at_max(sz, window.max[2], world_bounds.max[2])
        };
        let fresh = {
            let seed = |sx: i32, sy: i32, sz: i32| {
                if !seeded_cell(sx, sy, sz) {
                    return None;
                }
                Some((current.sky_at(sx, sy, sz), current.block_at(sx, sy, sz)))
            };
            crate::light::compute_seeded(window, &opaque, &emission, &self.heights, &seed)
        };

        // Blit the recomputed window back, recording which sections actually
        // changed as we go. Doing it per cell avoids allocating two 4 KiB
        // snapshots per section on every edit, which was most of the cost.
        let wb = fresh.bounds;
        let mut field = current;
        let mut changed_sections: Vec<(i32, i32, i32)> = Vec::new();
        for bz in wb.min[2]..wb.max[2] {
            for by in wb.min[1]..wb.max[1] {
                for bx in wb.min[0]..wb.max[0] {
                    if seeded_cell(bx, by, bz) {
                        continue;
                    }
                    let sky = fresh.sky_at(bx, by, bz);
                    let block = fresh.block_at(bx, by, bz);
                    if field.set_cell(bx, by, bz, sky, block) {
                        let key = (
                            bx.div_euclid(SECTION),
                            by.div_euclid(SECTION),
                            bz.div_euclid(SECTION),
                        );
                        if !changed_sections.contains(&key) {
                            changed_sections.push(key);
                        }
                    }
                }
            }
        }
        self.light = Some(field);
        self.light_dirty = false;
    }

    /// Re-bake dirty sections without rebuilding the flat frame.
    ///
    /// This is what a renderer with per-section buffers calls: it never touches
    /// the megabyte-sized contiguous copy, it only needs to know which sections
    /// moved. Returns the current revision, or 0 when nothing changed.
    pub fn rebuild_meshes(&mut self) -> u32 {
        let view = ViewKey {
            block: (
                self.player.pos[0].floor() as i32,
                self.player.pos[1].floor() as i32,
                self.player.pos[2].floor() as i32,
            ),
            radius: self.draw_radius,
        };
        if self.light_dirty {
            self.relight();
        } else if !self.light_edits.is_empty() {
            let edits = std::mem::take(&mut self.light_edits);
            for (x, y, z) in edits {
                self.relight_around(x, y, z);
            }
        }
        if !self.meshes.needs_work(view) {
            return 0;
        }
        self.frames += 1;
        self.bake_dirty_sections(view);
        self.revision = self.revision.wrapping_add(1).max(1);
        self.meshes.mark_assembled(view);
        self.revision
    }

    /// Visible sections as `(x, y, z, vertex_count, generation)`, sorted.
    pub fn visible_sections(&self) -> Vec<(i32, i32, i32, u32, u32)> {
        let view = ViewKey {
            block: (
                self.player.pos[0].floor() as i32,
                self.player.pos[1].floor() as i32,
                self.player.pos[2].floor() as i32,
            ),
            radius: self.draw_radius,
        };
        self.meshes
            .visible(view)
            .into_iter()
            .map(|(key, verts, gen)| (key.0, key.1, key.2, verts, gen))
            .collect()
    }

    /// Vertices of one section, for a renderer uploading per-section buffers.
    pub fn section_vertices(&self, key: (i32, i32, i32)) -> Option<&[f32]> {
        self.meshes.section_vertices(key)
    }

    /// Bytes the flat assembler would copy for the current view: the number the
    /// per-section path avoids re-copying every frame.
    pub fn flat_frame_bytes(&self) -> usize {
        let view = ViewKey {
            block: (
                self.player.pos[0].floor() as i32,
                self.player.pos[1].floor() as i32,
                self.player.pos[2].floor() as i32,
            ),
            radius: self.draw_radius,
        };
        self.meshes.visible_byte_len(view)
    }

    /// Per-section light bytes, for the incremental-vs-full equivalence tests.
    pub fn light_section_bytes(&self, key: (i32, i32, i32)) -> Option<(Vec<u8>, Vec<u8>)> {
        self.light.as_ref().and_then(|f| f.section_bytes(key))
    }

    /// Daylight factor; changing it re-bakes everything because the sky channel
    /// is baked into vertex colour.
    pub fn set_daylight(&mut self, daylight: f32) {
        let clamped = daylight.clamp(0.0, 1.0);
        if (clamped - self.daylight).abs() < 0.01 {
            return;
        }
        self.daylight = clamped;
        self.meshes.invalidate_all();
    }

    pub fn daylight(&self) -> f32 {
        self.daylight
    }

    /// Light level at a block position, as the mesher sees it.
    pub fn brightness_at(&self, x: i32, y: i32, z: i32) -> u8 {
        match &self.light {
            Some(f) => f.brightness(x, y, z, self.daylight),
            None => 15,
        }
    }

    /// Raw sky channel, ignoring daylight; surfaced for diagnostics and tests.
    pub fn sky_at(&self, x: i32, y: i32, z: i32) -> u8 {
        self.light.as_ref().map(|f| f.sky_at(x, y, z)).unwrap_or(15)
    }

    /// Raw block-light channel.
    pub fn light_block_at(&self, x: i32, y: i32, z: i32) -> u8 {
        self.light
            .as_ref()
            .map(|f| f.block_at(x, y, z))
            .unwrap_or(0)
    }

    fn invalidate_around(&mut self, x: i32, y: i32, z: i32) {
        let base = (
            x.div_euclid(SECTION),
            y.div_euclid(SECTION),
            z.div_euclid(SECTION),
        );
        self.meshes.mark_dirty(base);
        // A block on a section face changes the *neighbour's* culling too.
        let lx = x.rem_euclid(SECTION);
        let ly = y.rem_euclid(SECTION);
        let lz = z.rem_euclid(SECTION);
        if lx == 0 {
            self.meshes.mark_dirty((base.0 - 1, base.1, base.2));
        }
        if lx == SECTION - 1 {
            self.meshes.mark_dirty((base.0 + 1, base.1, base.2));
        }
        if ly == 0 {
            self.meshes.mark_dirty((base.0, base.1 - 1, base.2));
        }
        if ly == SECTION - 1 {
            self.meshes.mark_dirty((base.0, base.1 + 1, base.2));
        }
        if lz == 0 {
            self.meshes.mark_dirty((base.0, base.1, base.2 - 1));
        }
        if lz == SECTION - 1 {
            self.meshes.mark_dirty((base.0, base.1, base.2 + 1));
        }
    }

    /// Step every animated texture and dirty the sections that sample one whose
    /// frame changed. Returns how many sections were invalidated.
    pub fn advance_animations(&mut self) -> usize {
        if self.registry.animations.is_empty() {
            return 0;
        }
        let tick = self.ticks;
        let mut changed: Vec<u16> = Vec::new();
        for (slot, anim) in &self.registry.animations {
            // An interpolating texture changes every tick (the blend factor
            // moves), so it has to be re-baked each tick; a stepped one only
            // when the integer frame advances.
            if anim.interpolate {
                changed.push(*slot);
                continue;
            }
            let frame = anim.frame_at(tick);
            match self.anim_frames.get(slot) {
                Some(previous) if *previous == frame => {}
                _ => {
                    self.anim_frames.insert(*slot, frame);
                    changed.push(*slot);
                }
            }
        }
        let mut touched = 0;
        for slot in changed {
            touched += self.meshes.mark_texture_animated(slot);
        }
        touched
    }

    /// Queue a sound event at a world position. The engine resolves the event,
    /// picks a variant, attenuates by distance from the player and hands the
    /// result to the platform. Returns true when something was queued.
    pub fn play_sound(&mut self, event: &str, x: f32, y: f32, z: f32) -> bool {
        let id = match self.registry.sounds.event_id(event) {
            Some(id) => id,
            None => return false,
        };
        let p = self.player.pos;
        let dx = x - p[0];
        let dy = y - p[1];
        let dz = z - p[2];
        let distance = (dx * dx + dy * dy + dz * dz).sqrt();
        let gain = crate::sound::SoundCatalog::attenuation(distance);
        if gain <= 0.0 {
            return false;
        }
        match self.registry.sounds.request(id, gain) {
            Some(request) => {
                self.sounds.push(request);
                true
            }
            None => false,
        }
    }

    /// Next sound the platform should play.
    pub fn next_sound(&mut self) -> Option<crate::sound::SoundRequest> {
        self.sounds.pop()
    }

    pub fn pending_sounds(&self) -> usize {
        self.sounds.len()
    }

    /// Number of sound events the pack defines.
    pub fn sound_event_count(&self) -> usize {
        self.registry.sounds.event_count()
    }

    /// Number of animated textures with more than one frame.
    pub fn animated_texture_count(&self) -> usize {
        self.registry.animations.len()
    }

    /// Number of cached section meshes; used by diagnostics and tests.
    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }

    /// Quads baked for one section, or `None` when it has no mesh yet.
    pub fn mesh_quad_count(&self, key: (i32, i32, i32)) -> Option<u32> {
        self.meshes.get(key).map(|m| m.quads)
    }

    pub fn frame_quad_count(&self) -> u32 {
        self.frame.quads
    }

    fn solid(&self, x: i32, y: i32, z: i32) -> bool {
        self.world.get(x, y, z) != self.world.air
    }

    fn collides(&self, p: [f32; 3]) -> bool {
        let min = [
            (p[0] - HALF_W).floor() as i32,
            p[1].floor() as i32,
            (p[2] - HALF_W).floor() as i32,
        ];
        let max = [
            (p[0] + HALF_W).floor() as i32,
            (p[1] + HEIGHT).floor() as i32,
            (p[2] + HALF_W).floor() as i32,
        ];
        for x in min[0]..=max[0] {
            for y in min[1]..=max[1] {
                for z in min[2]..=max[2] {
                    if self.solid(x, y, z) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Voxel raycast (Amanatides & Woo). Returns the first solid block within
    /// `max_distance`, or `None`. Direction is normalised here, so callers can
    /// pass an unnormalised camera vector.
    pub fn raycast(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        max_distance: f32,
    ) -> Option<RayHit> {
        let len = (direction[0] * direction[0]
            + direction[1] * direction[1]
            + direction[2] * direction[2])
            .sqrt();
        if len < 1e-6 || max_distance <= 0.0 || max_distance.is_nan() {
            return None;
        }
        let d = [direction[0] / len, direction[1] / len, direction[2] / len];

        let mut x = origin[0].floor() as i32;
        let mut y = origin[1].floor() as i32;
        let mut z = origin[2].floor() as i32;

        let step = [
            if d[0] > 0.0 { 1 } else { -1 },
            if d[1] > 0.0 { 1 } else { -1 },
            if d[2] > 0.0 { 1 } else { -1 },
        ];
        let t_delta = [
            if d[0].abs() < 1e-6 {
                f32::INFINITY
            } else {
                (1.0 / d[0]).abs()
            },
            if d[1].abs() < 1e-6 {
                f32::INFINITY
            } else {
                (1.0 / d[1]).abs()
            },
            if d[2].abs() < 1e-6 {
                f32::INFINITY
            } else {
                (1.0 / d[2]).abs()
            },
        ];
        // distance along the ray to the first voxel boundary on each axis
        let mut t_max = [
            if d[0].abs() < 1e-6 {
                f32::INFINITY
            } else {
                let boundary = if step[0] > 0 {
                    (x + 1) as f32
                } else {
                    x as f32
                };
                (boundary - origin[0]) / d[0]
            },
            if d[1].abs() < 1e-6 {
                f32::INFINITY
            } else {
                let boundary = if step[1] > 0 {
                    (y + 1) as f32
                } else {
                    y as f32
                };
                (boundary - origin[1]) / d[1]
            },
            if d[2].abs() < 1e-6 {
                f32::INFINITY
            } else {
                let boundary = if step[2] > 0 {
                    (z + 1) as f32
                } else {
                    z as f32
                };
                (boundary - origin[2]) / d[2]
            },
        ];

        // The camera may start inside a block (head in a wall): ignore the
        // starting voxel, which is what survival mode effectively does too.
        let mut travelled = 0.0f32;
        while travelled <= max_distance {
            let axis = if t_max[0] <= t_max[1] && t_max[0] <= t_max[2] {
                0
            } else if t_max[1] <= t_max[2] {
                1
            } else {
                2
            };
            travelled = t_max[axis];
            if travelled > max_distance {
                break;
            }
            let normal = match axis {
                0 => {
                    x += step[0];
                    (-step[0] as i8, 0, 0)
                }
                1 => {
                    y += step[1];
                    (0, -step[1] as i8, 0)
                }
                _ => {
                    z += step[2];
                    (0, 0, -step[2] as i8)
                }
            };
            t_max[axis] += t_delta[axis];
            if self.solid(x, y, z) {
                return Some(RayHit {
                    block: (x, y, z),
                    normal,
                    distance: travelled,
                });
            }
        }
        None
    }

    /// Air id, for clearing blocks.
    pub fn air(&self) -> u16 {
        self.world.air
    }

    /// Break the block a ray hit. Returns the old id, or `None` when the ray
    /// missed.
    pub fn break_block(&mut self, hit: RayHit) -> Option<u16> {
        let (x, y, z) = hit.block;
        let old = self.world.get(x, y, z);
        if old == self.world.air {
            return None;
        }
        self.set_block(x, y, z, self.world.air);
        Some(old)
    }

    /// Place `id` against the hit face unless something already occupies it.
    /// Returns whether a block was written.
    pub fn place_block(&mut self, hit: RayHit, id: u16) -> bool {
        let (x, y, z) = hit.adjacent();
        if self.world.get(x, y, z) != self.world.air {
            return false;
        }
        // Do not entomb the player: refuse to place inside their capsule.
        let p = self.player.pos;
        let overlaps = (x as f32) < p[0] + HALF_W
            && (x as f32 + 1.0) > p[0] - HALF_W
            && (y as f32) < p[1] + HEIGHT
            && (y as f32 + 1.0) > p[1]
            && (z as f32) < p[2] + HALF_W
            && (z as f32 + 1.0) > p[2] - HALF_W;
        if overlaps {
            return false;
        }
        self.set_block(x, y, z, id);
        true
    }

    pub fn tick(&mut self, dt: f32) {
        let dt = dt.clamp(0.0, 0.1);
        self.ticks += 1;
        self.advance_animations();

        if !self.player.flying {
            self.player.vel[1] += GRAVITY * dt;
        }
        let fwd = [-self.player.yaw.sin(), 0.0, -self.player.yaw.cos()];
        let right = [fwd[2], 0.0, -fwd[0]];
        let mut wish = [0.0f32; 3];
        wish[0] += fwd[0] + right[0] * 0.0;
        wish[2] += fwd[2] + right[2] * 0.0;
        let wish_len = (wish[0] * wish[0] + wish[2] * wish[2]).sqrt();
        if wish_len > 0.0 {
            wish[0] = wish[0] / wish_len * WALK;
            wish[2] = wish[2] / wish_len * WALK;
        }
        self.player.vel[0] = wish[0];
        self.player.vel[2] = wish[2];

        // axis-separated movement so a wall stops only one axis
        for axis in 0..3 {
            let mut next = self.player.pos;
            next[axis] += self.player.vel[axis] * dt;
            if !self.collides(next) {
                self.player.pos = next;
            } else {
                if axis == 1 {
                    if self.player.vel[1] < 0.0 {
                        self.player.on_ground = true;
                    }
                    self.player.vel[1] = 0.0;
                } else {
                    self.player.vel[axis] = 0.0;
                }
            }
        }
        if self.player.pos[1] < -8.0 {
            self.player.pos = [0.5, 2.0, 0.5];
            self.player.vel = [0.0; 3];
        }
        if self.player.pos[1] > 0.0
            && !self.collides([
                self.player.pos[0],
                self.player.pos[1] - 0.01,
                self.player.pos[2],
            ])
        {
            self.player.on_ground = false;
        }
    }

    /// Rebuild the frame from the section cache.
    ///
    /// Returns `true` when the vertex buffer changed. Sections are re-baked
    /// only when they were invalidated by an edit; an untouched world costs one
    /// `ViewKey` comparison and returns `false`.
    pub fn rebuild_frame(&mut self) -> bool {
        let view = ViewKey {
            block: (
                self.player.pos[0].floor() as i32,
                self.player.pos[1].floor() as i32,
                self.player.pos[2].floor() as i32,
            ),
            radius: self.draw_radius,
        };
        if self.light_dirty {
            self.relight();
        } else if !self.light_edits.is_empty() {
            let edits = std::mem::take(&mut self.light_edits);
            for (x, y, z) in edits {
                self.relight_around(x, y, z);
            }
        }
        if !self.meshes.needs_work(view) {
            return false;
        }
        self.frames += 1;

        self.bake_dirty_sections(view);

        self.meshes.assemble(view, &mut self.frame);
        self.revision = self.revision.wrapping_add(1).max(1);
        true
    }

    /// Bake every dirty section that is in view.
    ///
    /// Shared by the flat assembler and the per-section path: both need the
    /// meshes up to date, only the delivery differs.
    fn bake_dirty_sections(&mut self, view: ViewKey) {
        let r = self.draw_radius * SECTION + SECTION;
        let (px, py, pz) = view.block;
        let in_view = |sx: i32, sy: i32, sz: i32| {
            let cx = sx * SECTION;
            let cy = sy * SECTION;
            let cz = sz * SECTION;
            cx + SECTION >= px - r
                && cx <= px + r
                && cy + SECTION >= py - r
                && cy <= py + r
                && cz + SECTION >= pz - r
                && cz <= pz + r
        };

        let dirty: Vec<(i32, i32, i32)> = self
            .world
            .sections()
            .filter(|((sx, sy, sz), _)| {
                if !in_view(*sx, *sy, *sz) {
                    return false;
                }
                match self.meshes.get((*sx, *sy, *sz)) {
                    Some(mesh) => mesh.dirty,
                    None => true,
                }
            })
            .map(|(key, _)| *key)
            .collect();

        for key in dirty {
            let chunk = match self.world.sections().find(|(k, _)| **k == key) {
                Some((_, chunk)) => chunk,
                None => continue,
            };
            let mut scratch = std::mem::take(self.meshes.entry(key));
            {
                let neighbour = |x: i32, y: i32, z: i32| self.world.get(x, y, z);
                let opaque =
                    |x: i32, y: i32, z: i32| self.registry.is_opaque(self.world.get(x, y, z));
                let input = BakeInput {
                    key,
                    chunk,
                    registry: &self.registry,
                    air: self.world.air,
                    tick: self.ticks,
                    neighbour: &neighbour,
                    light: self.light.as_ref(),
                    daylight: self.daylight,
                    opaque: &opaque,
                };
                bake_section(&input, &mut scratch);
            }
            scratch.generation = self.next_generation;
            self.next_generation = self.next_generation.wrapping_add(1).max(1);
            *self.meshes.entry(key) = scratch;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hz_core::{kind, Buf, Pack};

    fn test_pack() -> Vec<u8> {
        // one cube model with 6 faces, all cullable, and stone/air blocks
        let mut md = Buf::new();
        md.bytes(b"MD2");
        md.u16(1);
        md.str16("all");
        md.str16("minecraft:block/stone");
        md.u16(6);
        for face in 0..6u8 {
            md.u8(face);
            md.u8(face + 1);
            md.i16(-1);
            md.u8(0);
            md.u16(0);
            for c in 0..4u8 {
                md.f32(c as f32 * 0.2);
                md.f32(0.0);
                md.f32(0.0);
            }
            for _ in 0..4 {
                md.f32(0.0);
                md.f32(0.0);
            }
        }
        let mut bs = Buf::new();
        bs.bytes(b"BS1");
        bs.u32(1);
        bs.u8(0);
        bs.u8(0);
        bs.u8(0);
        bs.u8(1);
        bs.u16(0);
        bs.str16("minecraft:block/stone");

        let mut p = Pack::new();
        p.add(kind::MODEL, "models/minecraft:block/cube", md.finish());
        p.add(
            kind::BLOCKSTATES,
            "blockstates/minecraft:stone",
            bs.finish(),
        );
        p.write()
    }

    #[test]
    fn world_and_frame() {
        let mut g = Game::new(&test_pack()).unwrap();
        g.draw_radius = 1;
        g.rebuild_frame();
        // the pad is 17×17 blocks; the top face of every block is unculled
        assert!(g.frame().quads > 100, "quads={}", g.frame().quads);
        assert_eq!(
            g.frame().verts.len(),
            g.frame().quads as usize * 6 * crate::frame::FLOATS_PER_VERTEX
        );
        assert_eq!(g.world.get(0, 0, 0), g.registry.stone);
    }

    #[test]
    fn culling_reduces_quads() {
        let bytes = test_pack();
        let mut open = Game::new(&bytes).unwrap();
        open.draw_radius = 1;
        open.world = {
            let mut w = World::new(open.registry.air);
            w.set(0, 0, 0, open.registry.stone);
            w
        };
        open.rebuild_frame();
        let lonely = open.frame().quads;

        let mut covered = Game::new(&bytes).unwrap();
        covered.draw_radius = 1;
        covered.world = {
            let mut w = World::new(covered.registry.air);
            for x in -1..=1 {
                for y in -1..=1 {
                    for z in -1..=1 {
                        w.set(x, y, z, covered.registry.stone);
                    }
                }
            }
            w
        };
        covered.rebuild_frame();
        // one isolated block: all six faces visible
        assert_eq!(lonely, 6);
        // 3×3×3 of solid stone: only the six outer shells survive culling,
        // 9 blocks per side, and the centre block contributes nothing.
        assert_eq!(covered.frame().quads, 54);
    }

    /// Pack with stone, air, torch and glass, all using one cube model.
    /// Enough to exercise opacity and emission.
    fn lit_pack() -> Vec<u8> {
        let mut md = Buf::new();
        md.bytes(b"MD2");
        md.u16(1);
        md.str16("all");
        md.str16("minecraft:block/stone");
        md.u16(6);
        for face in 0..6u8 {
            md.u8(face);
            md.u8(face + 1);
            md.i16(-1);
            md.u8(0);
            md.u16(0);
            for c in 0..4u8 {
                md.f32(c as f32 * 0.2);
                md.f32(0.0);
                md.f32(0.0);
            }
            for _ in 0..4 {
                md.f32(0.0);
                md.f32(0.0);
            }
        }
        let mut p = Pack::new();
        p.add(kind::MODEL, "models/minecraft:block/cube", md.finish());
        for name in [
            "minecraft:air",
            "minecraft:stone",
            "minecraft:torch",
            "minecraft:glass",
        ] {
            let mut bs = Buf::new();
            bs.bytes(b"BS1");
            bs.u32(1);
            bs.u8(0);
            bs.u8(0);
            bs.u8(0);
            bs.u8(1);
            bs.u16(0);
            bs.str16("minecraft:block/cube");
            p.add(
                kind::BLOCKSTATES,
                format!("blockstates/{name}"),
                bs.finish(),
            );
        }
        p.write()
    }

    #[test]
    fn windowed_relight_matches_a_full_recompute() {
        let bytes = lit_pack();
        let mut g = Game::new(&bytes).unwrap();
        let stone = g.registry.stone;
        let torch = g.registry.id("minecraft:torch").unwrap();
        let glass = g.registry.id("minecraft:glass").unwrap();
        let air = g.registry.air;
        g.world = World::new(air);
        // A floor with a couple of walls, so edits have neighbours to interact with.
        for x in -12i32..12 {
            for z in -12i32..12 {
                g.world.set(x, 0, z, stone);
            }
        }
        for y in 1..5 {
            for x in -12i32..12 {
                g.world.set(x, y, -6, stone);
            }
        }
        g.recompute_heights();
        g.relight();

        // A mixed edit sequence: torches, glass and holes, including boundary cells.
        let edits: [(i32, i32, i32, u16); 8] = [
            (0, 1, 0, torch),
            (3, 1, 3, glass),
            (-4, 1, 0, torch),
            (0, 0, 0, air),
            (5, 1, -6, air),
            (-6, 1, -6, torch),
            (2, 2, 0, stone),
            (0, 1, 0, air),
        ];
        for (x, y, z, id) in edits {
            g.set_block(x, y, z, id);
            // Windowed path: this is what the frame loop calls.
            assert!(g.rebuild_frame(), "an edit must repaint the frame");
        }

        // Snapshot the incrementally maintained field, then rebuild it from
        // scratch and compare cell by cell. A digest comparison here would hide
        // *where* the two diverge, which is what we need when this fails.
        let mut incremental: Vec<(i32, i32, i32, u8, u8)> = Vec::new();
        for (key, chunk) in g.world.sections() {
            let (sx, sy, sz) = *key;
            for ly in 0..16 {
                for lz in 0..16 {
                    for lx in 0..16 {
                        let (x, y, z) = (sx * 16 + lx, sy * 16 + ly, sz * 16 + lz);
                        let _ = chunk.get(lx as usize, ly as usize, lz as usize);
                        incremental.push((x, y, z, g.sky_at(x, y, z), g.light_block_at(x, y, z)));
                    }
                }
            }
        }
        g.relight();
        let mut mismatches = Vec::new();
        for (x, y, z, sky, block) in &incremental {
            let now = (g.sky_at(*x, *y, *z), g.light_block_at(*x, *y, *z));
            if now != (*sky, *block) {
                mismatches.push((*x, *y, *z, (*sky, *block), now));
            }
        }
        assert!(
            mismatches.is_empty(),
            "windowed vs full relight differ at {} cells, first five: {:?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(5)]
        );
    }

    #[test]
    fn torch_lights_a_room_with_falloff() {
        let bytes = lit_pack();
        let mut g = Game::new(&bytes).unwrap();
        let stone = g.registry.stone;
        let torch = g.registry.id("minecraft:torch").unwrap();
        let air = g.registry.air;
        g.world = World::new(air);
        // 5×5×5 room: solid shell, hollow inside
        for x in -3i32..=3 {
            for y in -1i32..=4 {
                for z in -3i32..=3 {
                    let shell = x.abs() == 3 || z.abs() == 3 || y == -1 || y == 4;
                    if shell {
                        g.world.set(x, y, z, stone);
                    }
                }
            }
        }
        g.world.set(0, 1, 0, torch);
        g.recompute_heights();
        g.relight();

        assert_eq!(g.light_block_at(0, 1, 0), 14, "a torch emits 14");
        assert_eq!(g.brightness_at(0, 1, 0), 14, "the torch cell itself");
        assert_eq!(g.brightness_at(0, 2, 0), 13, "one step away");
        assert_eq!(g.brightness_at(0, 3, 0), 12, "two steps away");
        assert_eq!(g.brightness_at(2, 1, 0), 12, "horizontal falloff");
        // outside the sealed shell there is no block light
        assert_eq!(g.brightness_at(5, 1, 5), 15, "open sky outside");
        assert_eq!(g.light_block_at(3, 1, 3), 0, "the wall blocks it");
    }

    #[test]
    fn skylight_keeps_its_level_down_a_shaft() {
        let bytes = lit_pack();
        let mut g = Game::new(&bytes).unwrap();
        let stone = g.registry.stone;
        let air = g.registry.air;
        g.world = World::new(air);
        // a 5×5 block of rock with a 1×1 shaft through it
        for x in -2i32..=2 {
            for z in -2i32..=2 {
                for y in 0i32..8 {
                    if x == 0 && z == 0 {
                        continue;
                    }
                    g.world.set(x, y, z, stone);
                }
            }
        }
        g.recompute_heights();
        g.relight();
        assert_eq!(g.sky_at(0, 0, 0), 15, "shaft bottom stays fully lit");
        assert_eq!(g.sky_at(0, 1, 0), 15);
        assert_eq!(g.sky_at(0, 7, 0), 15);
        // one step into the rock would be inside a solid block, so check the
        // neighbouring shaft-less column instead
        assert_eq!(g.sky_at(1, 3, 1), 0, "under 7 blocks of rock it is dark");
    }

    #[test]
    fn glass_transmits_skylight_and_rock_does_not() {
        let bytes = lit_pack();
        let mut g = Game::new(&bytes).unwrap();
        let stone = g.registry.stone;
        let glass = g.registry.id("minecraft:glass").unwrap();
        let air = g.registry.air;
        g.world = World::new(air);
        // roof at y=4: half stone, half glass
        for x in -6i32..=6 {
            for z in -6i32..=6 {
                let roof = if x < 0 { stone } else { glass };
                g.world.set(x, 4, z, roof);
            }
        }
        g.recompute_heights();
        g.relight();
        // Under glass the column is unobstructed, so it is fully lit; that light
        // then bleeds sideways into the shadowed half one level per block.
        assert_eq!(g.sky_at(2, 3, 0), 15, "under glass");
        assert_eq!(g.sky_at(1, 3, 0), 15);
        assert_eq!(g.sky_at(-1, 3, 0), 14, "one step into the shadow");
        assert_eq!(g.sky_at(-2, 3, 0), 13, "two steps into the shadow");
        // The roof spans x,z in -6..=6, so light also enters from the open sides
        // and the two fronts meet in the middle. This is the real solver output:
        // a symmetric -1-per-block falloff to a floor of 12 under the roof centre.
        let row: Vec<u8> = (-6..=6).map(|x| g.sky_at(x, 3, 0)).collect();
        assert_eq!(
            row,
            vec![14, 13, 12, 12, 13, 14, 15, 15, 15, 15, 15, 15, 15]
        );
        assert!(
            row[..6].iter().all(|v| *v < 15),
            "the stone half is darker than the glass half"
        );
    }

    #[test]
    fn daylight_scales_sky_but_not_block_light() {
        let bytes = lit_pack();
        let mut g = Game::new(&bytes).unwrap();
        let stone = g.registry.stone;
        let torch = g.registry.id("minecraft:torch").unwrap();
        let air = g.registry.air;
        g.world = World::new(air);
        for x in -20i32..=20 {
            for z in -20i32..=20 {
                g.world.set(x, 0, z, stone);
            }
        }
        // A sealed room (floor, four walls, roof) so the torch sits under zero
        // sky light; otherwise light bleeds in from the open sides.
        for x in 0i32..5 {
            for z in 0i32..5 {
                g.world.set(x, 6, z, stone);
                for y in 1..6 {
                    let edge = x == 0 || x == 4 || z == 0 || z == 4;
                    if edge {
                        g.world.set(x, y, z, stone);
                    }
                }
            }
        }
        g.world.set(2, 1, 2, torch);
        g.recompute_heights();
        g.relight();

        // Far enough that a torch's 14 levels cannot reach: sky only.
        let sky_only = (18, 2, 0);
        assert_eq!(g.light_block_at(sky_only.0, sky_only.1, sky_only.2), 0);
        assert_eq!(g.brightness_at(sky_only.0, sky_only.1, sky_only.2), 15);
        assert_eq!(g.sky_at(2, 2, 2), 0, "the room has no sky light");
        assert_eq!(g.brightness_at(2, 2, 2), 13, "one step above the torch");

        g.set_daylight(0.0);
        g.relight();
        assert_eq!(
            g.brightness_at(sky_only.0, sky_only.1, sky_only.2),
            0,
            "night removes the sky channel"
        );
        assert_eq!(g.brightness_at(2, 2, 2), 13, "the torch still burns");
    }

    #[test]
    fn ambient_occlusion_darkens_the_inner_corner() {
        let bytes = lit_pack();
        let mut g = Game::new(&bytes).unwrap();
        let stone = g.registry.stone;
        let air = g.registry.air;
        g.world = World::new(air);
        // floor plus two walls meeting at a corner
        for x in 0i32..4 {
            for z in 0i32..4 {
                g.world.set(x, 0, z, stone);
            }
        }
        for y in 1i32..3 {
            g.world.set(0, y, 0, stone);
            g.world.set(0, y, 1, stone);
            g.world.set(1, y, 0, stone);
        }
        g.recompute_heights();
        g.draw_radius = 1;
        g.meshes = crate::mesh::MeshCache::default();
        g.rebuild_frame();
        let verts = g.frame().verts.clone();
        assert!(!verts.is_empty());
        // vertex colour is floats 8..11; corners of the enclosed cell must be
        // darker than the fully open floor far from the walls
        let colours: Vec<f32> = verts.chunks(16).map(|v| v[8]).collect();
        let darkest = colours.iter().cloned().fold(f32::INFINITY, f32::min);
        let brightest = colours.iter().cloned().fold(0.0f32, f32::max);
        assert!(
            brightest - darkest > 0.1,
            "AO must produce a visible range: {darkest} .. {brightest}"
        );
    }

    #[test]
    fn placing_a_torch_changes_the_frame() {
        let bytes = lit_pack();
        let mut g = Game::new(&bytes).unwrap();
        let stone = g.registry.stone;
        let torch = g.registry.id("minecraft:torch").unwrap();
        let air = g.registry.air;
        g.world = World::new(air);
        for x in -2i32..=2 {
            for z in -2i32..=2 {
                g.world.set(x, 0, z, stone);
            }
        }
        g.recompute_heights();
        g.draw_radius = 1;
        g.meshes = crate::mesh::MeshCache::default();
        g.rebuild_frame();
        let before = g.frame().verts.clone();

        g.set_block(0, 1, 0, torch);
        assert!(g.rebuild_frame(), "a light source must repaint the frame");
        assert_ne!(g.frame().verts, before);
    }

    /// Pack with one animated texture: a 4-frame vertical strip.
    fn animated_pack() -> Vec<u8> {
        use hz_core::anim::Animation;
        let mut md = Buf::new();
        md.bytes(b"MD2");
        md.u16(1);
        md.str16("all");
        md.str16("minecraft:block/lava");
        md.u16(1);
        md.u8(0);
        md.u8(0);
        md.i16(-1);
        md.u8(0);
        md.u16(0);
        for _ in 0..4 {
            md.f32(0.0);
            md.f32(0.0);
            md.f32(0.0);
        }
        for uv in [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] {
            md.f32(uv[0]);
            md.f32(uv[1]);
        }

        let mut bs = Buf::new();
        bs.bytes(b"BS1");
        bs.u32(1);
        bs.u8(0);
        bs.u8(0);
        bs.u8(0);
        bs.u8(1);
        bs.u16(0);
        bs.str16("minecraft:block/lava");

        let anim = Animation {
            frametime: 1,
            indices: vec![0, 1, 2, 3],
            interpolate: false,
        };

        let mut p = Pack::new();
        p.add(kind::MODEL, "models/minecraft:block/lava", md.finish());
        p.add(
            kind::BLOCKSTATES,
            "blockstates/minecraft:block/lava",
            bs.finish(),
        );
        p.add(
            kind::TEXTURE,
            "textures/minecraft:block/lava",
            vec![1, 2, 3],
        );
        p.add(kind::ANIM, "anim/minecraft:block/lava", anim.to_bytes());
        p.write()
    }

    /// Same as `animated_pack`, but the animation asks for interpolation.
    fn animated_pack_interpolated() -> Vec<u8> {
        use hz_core::anim::Animation;
        let mut pack = Pack::read(&animated_pack()).unwrap();
        let anim = Animation {
            frametime: 2,
            indices: vec![0, 1, 2, 3],
            interpolate: true,
        };
        let mut rebuilt = Pack::new();
        for e in &pack.entries {
            if e.name == "anim/minecraft:block/lava" {
                rebuilt.add(hz_core::kind::ANIM, e.name.clone(), anim.to_bytes());
            } else {
                rebuilt.add(e.kind, e.name.clone(), e.data.clone());
            }
        }
        pack = rebuilt;
        pack.write()
    }

    #[test]
    fn animated_texture_changes_uvs_as_ticks_advance() {
        let bytes = animated_pack();
        let mut g = Game::new(&bytes).unwrap();
        assert_eq!(g.animated_texture_count(), 1);
        g.draw_radius = 1;
        g.world = World::new(g.registry.air);
        g.meshes = crate::mesh::MeshCache::default();
        g.world.set(0, 0, 0, g.registry.stone);
        g.set_block(0, 0, 0, g.registry.stone);
        g.rebuild_frame();
        let first = g.frame().verts.clone();
        assert!(!first.is_empty());

        // ticks 1..3 advance to frames 1..3, each with a different V offset
        let mut seen = vec![first];
        for _ in 0..3 {
            g.tick(1.0 / 20.0);
            assert!(g.rebuild_frame(), "an animation step must dirty the frame");
            seen.push(g.frame().verts.clone());
        }
        for i in 0..seen.len() {
            for j in (i + 1)..seen.len() {
                assert_ne!(seen[i], seen[j], "frames {i} and {j} must differ");
            }
        }
        // and it wraps: tick 4 is frame 0 again
        g.tick(1.0 / 20.0);
        g.rebuild_frame();
        assert_eq!(
            g.frame().verts,
            seen[0],
            "frame 0 must repeat after 4 ticks"
        );
    }

    #[test]
    fn interpolated_animation_blends_across_a_frame() {
        let bytes = animated_pack_interpolated();
        let mut g = Game::new(&bytes).unwrap();
        g.draw_radius = 1;
        g.world = World::new(g.registry.air);
        g.meshes = crate::mesh::MeshCache::default();
        g.set_block(0, 0, 0, g.registry.stone);
        g.rebuild_frame();

        // floats 13/14 of each vertex are v_delta and blend
        let sample = |game: &Game| -> (f32, f32) {
            let v = &game.frame().verts;
            (v[13], v[14])
        };
        let (delta0, blend0) = sample(&g);
        assert_eq!(blend0, 0.0, "tick 0 is the start of a frame");
        assert!(
            delta0 > 0.0,
            "the fade target is the next frame down the strip"
        );

        g.tick(1.0 / 20.0);
        assert!(g.rebuild_frame(), "interpolation must rebuild every tick");
        let (delta1, blend1) = sample(&g);
        assert_eq!(delta1, delta0, "the fade target does not move mid-frame");
        assert!(blend1 > blend0, "blend advances within the frame");

        // halfway through a 2-tick frame the blend is 0.5
        assert!((blend1 - 0.5).abs() < 1e-6, "blend was {blend1}");

        // a stepped animation never sets either field
        let stepped = animated_pack();
        let mut h = Game::new(&stepped).unwrap();
        h.draw_radius = 1;
        h.world = World::new(h.registry.air);
        h.meshes = crate::mesh::MeshCache::default();
        h.set_block(0, 0, 0, h.registry.stone);
        h.rebuild_frame();
        for _ in 0..3 {
            h.tick(1.0 / 20.0);
            h.rebuild_frame();
            let v = &h.frame().verts;
            assert_eq!(v[13], 0.0, "stepped animations carry no fade target");
            assert_eq!(v[14], 0.0, "stepped animations carry no blend");
        }
    }

    /// Pack with three blocks and one sound event backed by a real Ogg.
    fn sounding_pack() -> Vec<u8> {
        use crate::sound::testutil::{catalog_bytes, tiny_ogg};
        let mut pack = Pack::read(&lit_pack()).unwrap();
        let catalog = catalog_bytes(&[("block.stone.break", &[("minecraft:dig/stone1", 1)][..])]);
        pack.add(hz_core::kind::SOUND, "soundcatalog/minecraft", catalog);
        pack.add(
            hz_core::kind::SOUND,
            "sounds/minecraft:dig/stone1",
            tiny_ogg(44100),
        );
        pack.write()
    }

    #[test]
    fn sounds_attenuate_with_distance_and_queue() {
        let bytes = sounding_pack();
        let mut g = Game::new(&bytes).unwrap();
        assert_eq!(g.sound_event_count(), 1, "the catalog came through");
        g.player.pos = [0.5, 2.0, 0.5];

        // at the player's feet: full volume
        assert!(g.play_sound("block.stone.break", 0.5, 2.0, 0.5));
        let near = g.next_sound().expect("queued");
        assert!((near.volume - 1.0).abs() < 1e-6);
        assert_eq!(g.pending_sounds(), 0);

        // 8 blocks away: half volume
        assert!(g.play_sound("block.stone.break", 8.5, 2.0, 0.5));
        let mid = g.next_sound().unwrap();
        assert!((mid.volume - 0.5).abs() < 1e-5, "volume {}", mid.volume);

        // out of range: nothing queued
        assert!(!g.play_sound("block.stone.break", 40.0, 2.0, 0.5));
        assert_eq!(g.pending_sounds(), 0);

        // unknown event: nothing queued
        assert!(!g.play_sound("no.such.event", 0.5, 2.0, 0.5));
    }

    #[test]
    fn per_section_path_only_changes_the_edited_section() {
        let bytes = test_pack();
        let mut g = Game::new(&bytes).unwrap();
        g.draw_radius = 2;
        g.world = World::new(g.registry.air);
        g.meshes = crate::mesh::MeshCache::default();
        // two sections side by side so an edit can be localised
        for x in -20i32..20 {
            for z in -20i32..20 {
                g.set_block(x, 0, z, g.registry.stone);
            }
        }
        assert_ne!(g.rebuild_meshes(), 0, "first bake must report work");

        let before: Vec<(i32, i32, i32, u32, u32)> = g.visible_sections();
        assert!(before.len() >= 2, "sections: {before:?}");

        // Change one block in the *interior* of a section and look at what
        // moved. A boundary block would also re-bake its neighbours, because
        // their face culling changes — that is correct, not a leak.
        g.set_block(5, 1, 5, g.registry.stone);
        assert_ne!(g.rebuild_meshes(), 0);
        let after = g.visible_sections();

        let changed: Vec<_> = after
            .iter()
            .filter(|(x, y, z, _, gen)| {
                before
                    .iter()
                    .find(|(bx, by, bz, _, _)| (bx, by, bz) == (x, y, z))
                    .map(|(_, _, _, _, bgen)| bgen != gen)
                    .unwrap_or(true)
            })
            .collect();
        assert_eq!(
            changed.len(),
            1,
            "exactly one section should be re-baked, got {changed:?}"
        );
        assert_eq!(changed[0].0, 0, "the edited section is the one at x=0");
        assert_eq!(changed[0].2, 0);
        let _ = &after;
    }

    #[test]
    fn idle_section_path_reports_no_work() {
        let mut g = Game::new(&test_pack()).unwrap();
        assert_ne!(g.rebuild_meshes(), 0);
        for _ in 0..5 {
            assert_eq!(g.rebuild_meshes(), 0, "an untouched world must do nothing");
        }
    }

    #[test]
    fn section_vertices_add_up_to_the_flat_frame() {
        let mut g = Game::new(&test_pack()).unwrap();
        g.draw_radius = 1;
        g.world = World::new(g.registry.air);
        g.meshes = crate::mesh::MeshCache::default();
        for x in -4i32..4 {
            for z in -4i32..4 {
                g.set_block(x, 0, z, g.registry.stone);
            }
        }
        let rebuilt = g.rebuild_frame();
        assert!(rebuilt);
        // `verts` counts vertices; the buffer holds 16 floats per vertex.
        let flat = g.frame().vertex_count() as usize;
        let mut sectioned = 0usize;
        for (x, y, z, verts, _) in g.visible_sections() {
            let data = g.section_vertices((x, y, z)).expect("section data");
            assert_eq!(
                data.len(),
                verts as usize * crate::frame::FLOATS_PER_VERTEX,
                "section ({x},{y},{z}) length must be vertices × stride"
            );
            sectioned += verts as usize;
        }
        assert_eq!(
            sectioned, flat,
            "per-section bytes must equal the flat buffer"
        );
        assert_eq!(
            g.flat_frame_bytes(),
            flat * crate::frame::VERTEX_STRIDE,
            "flat byte count must match what the assembler would copy"
        );
    }

    #[test]
    fn idle_rebuild_reports_no_change() {
        let mut g = Game::new(&test_pack()).unwrap();
        g.draw_radius = 1;
        assert!(g.rebuild_frame(), "first build must produce a frame");
        let frames_after_first = g.frames;
        let quads = g.frame_quad_count();
        for _ in 0..10 {
            assert!(!g.rebuild_frame(), "an untouched world must not rebuild");
        }
        assert_eq!(g.frames, frames_after_first, "frame counter must not move");
        assert_eq!(g.frame_quad_count(), quads);
    }

    #[test]
    fn edit_invalidates_only_the_touched_sections_and_still_matches_full_bake() {
        let bytes = test_pack();
        let mut incremental = Game::new(&bytes).unwrap();
        incremental.draw_radius = 2;
        incremental.world = World::new(incremental.registry.air);
        incremental.meshes = crate::mesh::MeshCache::default();
        incremental.set_block(0, 0, 0, incremental.registry.stone);
        incremental.rebuild_frame();

        // Same world, built one edit at a time with rebuilds in between.
        for i in 0..24 {
            let x = i - 12;
            incremental.set_block(x, 0, 0, incremental.registry.stone);
            incremental.set_block(x, 1, 0, incremental.registry.stone);
            assert!(incremental.rebuild_frame());
        }

        // Reference: a fresh engine that applies every edit then bakes once.
        let mut reference = Game::new(&bytes).unwrap();
        reference.draw_radius = 2;
        reference.world = World::new(reference.registry.air);
        reference.meshes = crate::mesh::MeshCache::default();
        for i in 0..24 {
            let x = i - 12;
            reference.set_block(x, 0, 0, reference.registry.stone);
            reference.set_block(x, 1, 0, reference.registry.stone);
        }
        reference.player.pos = incremental.player.pos;
        reference.rebuild_frame();

        assert_eq!(
            incremental.frame_quad_count(),
            reference.frame_quad_count(),
            "incremental quad count must match a single full bake"
        );
        assert_eq!(
            incremental.frame().verts,
            reference.frame().verts,
            "incremental and full vertex buffers must be identical"
        );
    }

    #[test]
    fn breaking_a_boundary_block_reveals_the_neighbour_face() {
        let mut g = Game::new(&test_pack()).unwrap();
        g.draw_radius = 2;
        g.world = World::new(g.registry.air);
        g.meshes = crate::mesh::MeshCache::default();
        // two adjacent blocks straddling the x=15 / x=16 section boundary
        g.set_block(15, 0, 0, g.registry.stone);
        g.set_block(16, 0, 0, g.registry.stone);
        g.rebuild_frame();
        let before = g.frame_quad_count();

        g.set_block(15, 0, 0, g.registry.air);
        assert!(g.rebuild_frame());
        let after = g.frame_quad_count();

        // Removing the left block removes its 5 exposed faces and reveals one
        // new face on the block that is in the *other* section.
        assert_eq!(before, 10, "two touching blocks expose 5 faces each");
        assert_eq!(after, 6, "one block alone exposes 6 faces");
    }

    #[test]
    fn raycast_hits_the_pad_and_reports_the_face() {
        let mut g = Game::new(&test_pack()).unwrap();
        g.world = {
            let mut w = World::new(g.registry.air);
            for x in -2..=2 {
                for z in -2..=2 {
                    w.set(x, 0, z, g.registry.stone);
                }
            }
            w
        };
        // straight down from above the pad
        let hit = g.raycast([0.5, 4.0, 0.5], [0.0, -1.0, 0.0], 8.0).unwrap();
        assert_eq!(hit.block, (0, 0, 0));
        assert_eq!(hit.normal, (0, 1, 0));
        assert!(
            (hit.distance - 3.0).abs() < 1e-3,
            "distance {}",
            hit.distance
        );
        assert_eq!(hit.adjacent(), (0, 1, 0));

        // sideways into a wall: the face normal must point back at the ray
        let hit = g.raycast([-4.5, 0.5, 0.5], [1.0, 0.0, 0.0], 16.0).unwrap();
        assert_eq!(hit.block, (-2, 0, 0));
        assert_eq!(hit.normal, (-1, 0, 0));
    }

    #[test]
    fn raycast_misses_when_nothing_is_in_range() {
        let g = Game::new(&test_pack()).unwrap();
        assert!(g.raycast([0.5, 40.0, 0.5], [0.0, 1.0, 0.0], 8.0).is_none());
        assert!(g.raycast([0.5, 4.0, 0.5], [0.0, -1.0, 0.0], 0.0).is_none());
    }

    #[test]
    fn break_then_place_round_trips() {
        let mut g = Game::new(&test_pack()).unwrap();
        g.world = {
            let mut w = World::new(g.registry.air);
            w.set(0, 0, 0, g.registry.stone);
            w
        };
        let hit = g.raycast([0.5, 3.0, 0.5], [0.0, -1.0, 0.0], 8.0).unwrap();

        assert_eq!(g.break_block(hit), Some(g.registry.stone));
        assert_eq!(g.world.get(0, 0, 0), g.registry.air);

        // nothing left to break
        assert_eq!(g.break_block(hit), None);

        // placing against the top face of the removed block now targets air,
        // so pick a hit on a fresh block again
        let mut g2 = Game::new(&test_pack()).unwrap();
        g2.world = {
            let mut w = World::new(g2.registry.air);
            w.set(0, 0, 0, g2.registry.stone);
            w
        };
        let hit = g2.raycast([0.5, 3.0, 0.5], [0.0, -1.0, 0.0], 8.0).unwrap();
        g2.player.pos = [6.0, 3.0, 6.0]; // away from the target cell
        assert!(g2.place_block(hit, g2.registry.stone));
        assert_eq!(g2.world.get(0, 1, 0), g2.registry.stone);
        // occupied now
        assert!(!g2.place_block(hit, g2.registry.stone));
    }

    #[test]
    fn place_refuses_to_entomb_the_player() {
        let mut g = Game::new(&test_pack()).unwrap();
        g.world = {
            let mut w = World::new(g.registry.air);
            w.set(0, 0, 0, g.registry.stone);
            w
        };
        let hit = g.raycast([0.5, 3.0, 0.5], [0.0, -1.0, 0.0], 8.0).unwrap();
        // stand exactly where the new block would go
        g.player.pos = [0.5, 1.0, 0.5];
        assert!(!g.place_block(hit, g.registry.stone));
        assert_eq!(g.world.get(0, 1, 0), g.registry.air);
    }

    #[test]
    fn player_falls_to_ground() {
        let mut g = Game::new(&test_pack()).unwrap();
        for _ in 0..120 {
            g.tick(1.0 / 60.0);
        }
        assert!(g.player.pos[1] >= 0.5, "pos={:?}", g.player.pos);
        assert!(g.player.pos[1] < 3.0);
    }
}
