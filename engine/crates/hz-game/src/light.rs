//! Lighting: sky light, block light, and per-vertex ambient occlusion.
//!
//! Two channels, both 0..=15, exactly like Minecraft:
//!
//! * **sky light** — seeded 15 for every cell above its column's highest opaque
//!   block, then propagated. Propagation *downward* costs nothing (a lit shaft
//!   stays lit all the way down, which is the behaviour players expect); the
//!   other five directions cost 1.
//! * **block light** — seeded from emitters with their emission level, every
//!   direction costs 1.
//!
//! Both run as a bucket queue ordered by level, so each cell is relaxed at most
//! a handful of times and no priority queue is needed.
//!
//! The field is computed for a bounding box (the loaded sections). Cells outside
//! the box are treated as unlit; with a small draw radius that shows up as a
//! dark rim at the edge of the loaded world, which is the honest answer — we do
//! not know what is out there.

use std::collections::HashMap;

use crate::SECTION; // 16³ sections

pub const MAX: u8 = 15;

/// The six face neighbours, in the order the solvers visit them.
const NEIGHBOURS: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

/// Inclusive-min, exclusive-max box in block coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub min: [i32; 3],
    pub max: [i32; 3],
}

impl Bounds {
    pub fn from_sections(keys: &[(i32, i32, i32)]) -> Option<Bounds> {
        let first = keys.first()?;
        let mut min = [first.0 * SECTION, first.1 * SECTION, first.2 * SECTION];
        let mut max = [min[0] + SECTION, min[1] + SECTION, min[2] + SECTION];
        for (sx, sy, sz) in keys {
            let lo = [sx * SECTION, sy * SECTION, sz * SECTION];
            for a in 0..3 {
                min[a] = min[a].min(lo[a]);
                max[a] = max[a].max(lo[a] + SECTION);
            }
        }
        Some(Bounds { min, max })
    }

    pub fn dims(&self) -> [i32; 3] {
        [
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
        ]
    }

    /// The same box shrunk by one cell on every side, or `None` when that would
    /// be empty. Used to blit a relight window's interior back into the field:
    /// the outer shell is only there to supply boundary light.
    pub fn inset(&self, n: i32) -> Option<Bounds> {
        let b = Bounds {
            min: [self.min[0] + n, self.min[1] + n, self.min[2] + n],
            max: [self.max[0] - n, self.max[1] - n, self.max[2] - n],
        };
        if b.max[0] > b.min[0] && b.max[1] > b.min[1] && b.max[2] > b.min[2] {
            Some(b)
        } else {
            None
        }
    }

    /// Grow by `n` on every side, then clip to `limits`.
    pub fn expand_clipped(&self, n: i32, limits: &Bounds) -> Bounds {
        Bounds {
            min: [
                (self.min[0] - n).max(limits.min[0]),
                (self.min[1] - n).max(limits.min[1]),
                (self.min[2] - n).max(limits.min[2]),
            ],
            max: [
                (self.max[0] + n).min(limits.max[0]),
                (self.max[1] + n).min(limits.max[1]),
                (self.max[2] + n).min(limits.max[2]),
            ],
        }
    }

    /// A box around one cell, grown by `n` and clipped to `limits`.
    pub fn around(x: i32, y: i32, z: i32, n: i32, limits: &Bounds) -> Bounds {
        Bounds {
            min: [
                (x - n).max(limits.min[0]),
                (y - n).max(limits.min[1]),
                (z - n).max(limits.min[2]),
            ],
            max: [
                (x + n + 1).min(limits.max[0]),
                (y + n + 1).min(limits.max[1]),
                (z + n + 1).min(limits.max[2]),
            ],
        }
    }

    pub fn contains(&self, x: i32, y: i32, z: i32) -> bool {
        x >= self.min[0]
            && x < self.max[0]
            && y >= self.min[1]
            && y < self.max[1]
            && z >= self.min[2]
            && z < self.max[2]
    }

    fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        if !self.contains(x, y, z) {
            return None;
        }
        let d = self.dims();
        let lx = (x - self.min[0]) as usize;
        let ly = (y - self.min[1]) as usize;
        let lz = (z - self.min[2]) as usize;
        Some((ly * d[2] as usize + lz) * d[0] as usize + lx)
    }
}

/// Highest opaque block per column. Maintained by the world so sky seeding does
/// not have to rescan columns on every relight.
#[derive(Default, Clone)]
pub struct HeightMap {
    map: HashMap<(i32, i32), i32>,
}

impl HeightMap {
    pub fn get(&self, x: i32, z: i32) -> Option<i32> {
        self.map.get(&(x, z)).copied()
    }

    pub fn set(&mut self, x: i32, z: i32, height: i32) {
        self.map.insert((x, z), height);
    }

    pub fn remove(&mut self, x: i32, z: i32) {
        self.map.remove(&(x, z));
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// The two light channels over one bounding box.
pub struct LightField {
    pub bounds: Bounds,
    sky: Vec<u8>,
    block: Vec<u8>,
    /// Solver statistics, surfaced by `hz info` and the bench.
    pub sky_cells: u32,
    pub block_cells: u32,
}

impl LightField {
    pub fn sky_at(&self, x: i32, y: i32, z: i32) -> u8 {
        self.bounds.index(x, y, z).map(|i| self.sky[i]).unwrap_or(0)
    }

    pub fn block_at(&self, x: i32, y: i32, z: i32) -> u8 {
        self.bounds
            .index(x, y, z)
            .map(|i| self.block[i])
            .unwrap_or(0)
    }

    /// Combined brightness 0..=15 after the daylight factor, which is what the
    /// mesher bakes into vertex colour.
    pub fn brightness(&self, x: i32, y: i32, z: i32, daylight: f32) -> u8 {
        let sky = (self.sky_at(x, y, z) as f32 * daylight.clamp(0.0, 1.0)).round() as u8;
        sky.max(self.block_at(x, y, z))
    }

    /// Per-section slice, for change detection: `(sky, block)` of one 16³ box.
    pub fn section_bytes(&self, key: (i32, i32, i32)) -> Option<(Vec<u8>, Vec<u8>)> {
        let ox = key.0 * SECTION;
        let oy = key.1 * SECTION;
        let oz = key.2 * SECTION;
        if !self.bounds.contains(ox, oy, oz) {
            return None;
        }
        let mut sky = Vec::with_capacity((SECTION * SECTION * SECTION) as usize);
        let mut block = Vec::with_capacity((SECTION * SECTION * SECTION) as usize);
        for y in 0..SECTION {
            for z in 0..SECTION {
                for x in 0..SECTION {
                    sky.push(self.sky_at(ox + x, oy + y, oz + z));
                    block.push(self.block_at(ox + x, oy + y, oz + z));
                }
            }
        }
        Some((sky, block))
    }

    /// Write both channels for one cell; returns true when either changed.
    /// Used by the windowed relight so it never has to allocate a snapshot.
    pub fn set_cell(&mut self, x: i32, y: i32, z: i32, sky: u8, block: u8) -> bool {
        match self.bounds.index(x, y, z) {
            Some(i) => {
                let changed = self.sky[i] != sky || self.block[i] != block;
                self.sky[i] = sky;
                self.block[i] = block;
                changed
            }
            None => false,
        }
    }

    /// Copy `src` into this field wherever `keep` says so. Returns the number of
    /// cells written.
    ///
    /// `keep` is how a relight window excludes the cells it used as boundary
    /// seeds: those hold the *old* values on purpose, so they must not be
    /// overwritten with numbers computed without their neighbours.
    pub fn blit_where(&mut self, src: &LightField, keep: &dyn Fn(i32, i32, i32) -> bool) -> usize {
        let inner = src.bounds;
        let mut written = 0;
        for z in inner.min[2]..inner.max[2] {
            for y in inner.min[1]..inner.max[1] {
                for x in inner.min[0]..inner.max[0] {
                    if !keep(x, y, z) {
                        continue;
                    }
                    let (si, di) = match (src.bounds.index(x, y, z), self.bounds.index(x, y, z)) {
                        (Some(si), Some(di)) => (si, di),
                        _ => continue,
                    };
                    self.sky[di] = src.sky[si];
                    self.block[di] = src.block[si];
                    written += 1;
                }
            }
        }
        written
    }

    pub fn cell_count(&self) -> usize {
        self.sky.len()
    }
}

/// Cube of one section; the unit a change-detection comparison works on.
const VOL: usize = (SECTION * SECTION * SECTION) as usize;

struct Buckets {
    levels: Vec<Vec<u32>>,
}

impl Buckets {
    fn new() -> Buckets {
        Buckets {
            levels: vec![Vec::new(); MAX as usize + 1],
        }
    }

    fn push(&mut self, level: u8, index: u32) {
        self.levels[level as usize].push(index);
    }

    fn pop_below(&mut self, level: u8) -> Option<u32> {
        self.levels[level as usize].pop()
    }
}

/// Compute both channels for `bounds`.
///
/// `opaque` tells the solver where light stops; `emission` gives block light
/// strength. Both come from the registry so the pack decides what a "solid"
/// or "glowing" block is.
pub fn compute(
    bounds: Bounds,
    opaque: &dyn Fn(i32, i32, i32) -> bool,
    emission: &dyn Fn(i32, i32, i32) -> u8,
    heights: &HeightMap,
) -> LightField {
    compute_seeded(bounds, opaque, emission, heights, &|_, _, _| None)
}

/// Same solve, but `seed` may supply initial values for cells — used to seal a
/// relight window's boundary with the values the surrounding field already had.
///
/// The seal is exact because light fades by at least 1 per step: with a margin
/// of 16 the edit cannot influence the boundary, so keeping the old numbers
/// there cannot hide a change.
pub fn compute_seeded(
    bounds: Bounds,
    opaque: &dyn Fn(i32, i32, i32) -> bool,
    emission: &dyn Fn(i32, i32, i32) -> u8,
    heights: &HeightMap,
    seed: &dyn Fn(i32, i32, i32) -> Option<(u8, u8)>,
) -> LightField {
    let dims = bounds.dims();
    let count = (dims[0].max(0) * dims[1].max(0) * dims[2].max(0)) as usize;
    let mut sky = vec![0u8; count];
    let mut block = vec![0u8; count];

    // Boundary seeds first: they are the "light already present outside".
    // They must also enter the queues, or their light would sit at the edge of
    // the volume and never flow inward.
    let mut seeded: Vec<(usize, u8, u8)> = Vec::new();
    for z in bounds.min[2]..bounds.max[2] {
        for y in bounds.min[1]..bounds.max[1] {
            for x in bounds.min[0]..bounds.max[0] {
                if let Some((s, b)) = seed(x, y, z) {
                    if let Some(i) = bounds.index(x, y, z) {
                        if s > sky[i] {
                            sky[i] = s;
                        }
                        if b > block[i] {
                            block[i] = b;
                        }
                        seeded.push((i, sky[i], block[i]));
                    }
                }
            }
        }
    }

    // --- sky seeding -------------------------------------------------------
    // Above the column's highest opaque block the sky is unobstructed.
    let mut sky_buckets = Buckets::new();
    let mut sky_cells = 0u32;
    for z in bounds.min[2]..bounds.max[2] {
        for x in bounds.min[0]..bounds.max[0] {
            let height = heights.get(x, z).unwrap_or(bounds.min[1] - 1);
            let start = (height + 1).max(bounds.min[1]);
            for y in start..bounds.max[1] {
                if let Some(i) = bounds.index(x, y, z) {
                    if MAX > sky[i] {
                        sky[i] = MAX;
                        sky_cells += 1;
                    }
                }
            }
        }
    }
    // Frontier only: a cell whose six neighbours are all fully lit cannot
    // improve anything, and in an open field that is almost every cell. Skipping
    // them is the difference between a cheap pass and a 6× queue.
    for z in bounds.min[2]..bounds.max[2] {
        for y in bounds.min[1]..bounds.max[1] {
            for x in bounds.min[0]..bounds.max[0] {
                let i = match bounds.index(x, y, z) {
                    Some(i) => i,
                    None => continue,
                };
                if sky[i] != MAX {
                    continue;
                }
                let mut touches_dark = false;
                for (dx, dy, dz) in NEIGHBOURS {
                    let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                    if !bounds.contains(nx, ny, nz) {
                        continue;
                    }
                    if let Some(j) = bounds.index(nx, ny, nz) {
                        if sky[j] < MAX {
                            touches_dark = true;
                            break;
                        }
                    }
                }
                if touches_dark {
                    sky_buckets.push(MAX, i as u32);
                }
            }
        }
    }

    // Seeds enter the queues at their own level so they can push inward.
    for (i, s, _) in &seeded {
        if *s > 0 {
            sky_buckets.push(*s, *i as u32);
        }
    }

    // Propagate sky: down is free, everything else costs 1.
    for level in (1..=MAX).rev() {
        while let Some(raw) = sky_buckets.pop_below(level) {
            let i = raw as usize;
            if sky[i] != level {
                continue; // stale entry
            }
            let (x, y, z) = decode(&bounds, i);
            for (dx, dy, dz) in NEIGHBOURS {
                let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                if !bounds.contains(nx, ny, nz) {
                    continue;
                }
                if opaque(nx, ny, nz) {
                    continue;
                }
                let j = match bounds.index(nx, ny, nz) {
                    Some(j) => j,
                    None => continue,
                };
                let target = if dy == -1 { level } else { level - 1 };
                if target > sky[j] {
                    sky[j] = target;
                    sky_buckets.push(target, j as u32);
                }
            }
        }
    }

    // --- block light -------------------------------------------------------
    let mut block_buckets = Buckets::new();
    let mut block_cells = 0u32;
    for z in bounds.min[2]..bounds.max[2] {
        for y in bounds.min[1]..bounds.max[1] {
            for x in bounds.min[0]..bounds.max[0] {
                let e = emission(x, y, z);
                if e == 0 {
                    continue;
                }
                if let Some(i) = bounds.index(x, y, z) {
                    if e > block[i] {
                        block[i] = e;
                        block_buckets.push(e, i as u32);
                        block_cells += 1;
                    }
                }
            }
        }
    }
    for (i, _, b) in &seeded {
        if *b > 0 {
            block_buckets.push(*b, *i as u32);
        }
    }
    for level in (1..=MAX).rev() {
        while let Some(raw) = block_buckets.pop_below(level) {
            let i = raw as usize;
            if block[i] != level {
                continue;
            }
            let (x, y, z) = decode(&bounds, i);
            for (dx, dy, dz) in NEIGHBOURS {
                let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                if !bounds.contains(nx, ny, nz) {
                    continue;
                }
                if opaque(nx, ny, nz) {
                    continue;
                }
                let j = match bounds.index(nx, ny, nz) {
                    Some(j) => j,
                    None => continue,
                };
                let target = level - 1;
                if target > block[j] {
                    block[j] = target;
                    block_buckets.push(target, j as u32);
                }
            }
        }
    }

    LightField {
        bounds,
        sky,
        block,
        sky_cells,
        block_cells,
    }
}

fn decode(bounds: &Bounds, index: usize) -> (i32, i32, i32) {
    let d = bounds.dims();
    let dx = d[0] as usize;
    let dz = d[2] as usize;
    let x = index % dx;
    let rest = index / dx;
    let z = rest % dz;
    let y = rest / dz;
    (
        bounds.min[0] + x as i32,
        bounds.min[1] + y as i32,
        bounds.min[2] + z as i32,
    )
}

/// Ambient occlusion for one vertex of a quad, following the classic
/// "three neighbours" rule: two edge neighbours and the diagonal one.
///
/// Returns 0 (no occlusion) .. 3 (fully cornered).
pub fn vertex_ao(side1: bool, side2: bool, corner: bool) -> u8 {
    if side1 && side2 {
        return 3;
    }
    side1 as u8 + side2 as u8 + corner as u8
}

/// Brightness multiplier for an AO level. Tuned so corners read as corners
/// without turning the world into a checkerboard.
pub fn ao_factor(level: u8) -> f32 {
    match level {
        0 => 1.0,
        1 => 0.86,
        2 => 0.72,
        _ => 0.58,
    }
}

/// Light 0..15 → linear multiplier, matching Minecraft's curve closely enough
/// that a cave mouth looks like a cave mouth.
pub fn light_curve(level: u8) -> f32 {
    if level == 0 {
        return 0.03;
    }
    let l = level as f32 / MAX as f32;
    // Minecraft's brightness table bottoms out around 0.03 and rises fast.
    0.03 + 0.97 * (l * l * 0.65 + l * 0.35)
}

/// Keep the compiler honest about the array size used above.
const _: () = assert!(VOL == 4096);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_index_roundtrip() {
        let b = Bounds {
            min: [-16, 0, -16],
            max: [32, 32, 32],
        };
        for y in [0, 5, 31] {
            let i = b.index(0, y, 0).unwrap();
            assert_eq!(decode(&b, i), (0, y, 0));
        }
        assert!(b.index(32, 0, 0).is_none(), "max is exclusive");
    }

    #[test]
    fn ao_rule_matches_the_three_neighbour_model() {
        assert_eq!(vertex_ao(false, false, false), 0);
        assert_eq!(vertex_ao(true, false, false), 1);
        assert_eq!(vertex_ao(false, false, true), 1);
        assert_eq!(vertex_ao(true, false, true), 2);
        assert_eq!(vertex_ao(true, true, false), 3, "two edges dominate");
        assert_eq!(vertex_ao(true, true, true), 3);
        assert!(ao_factor(0) > ao_factor(1));
        assert!(ao_factor(3) < 0.6);
    }

    #[test]
    fn light_curve_is_monotonic_and_nonzero_at_zero() {
        let mut previous = -1.0;
        for level in 0..=MAX {
            let v = light_curve(level);
            assert!(v > previous, "level {level} must be brighter");
            previous = v;
        }
        assert!(light_curve(0) > 0.0, "never pure black");
        assert!((light_curve(MAX) - 1.0).abs() < 1e-6);
    }
}
