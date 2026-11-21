//! Registry built from a compiled `.hzp`: block names, their state tables, and
//! the baked quad geometry, indexed by dense u16 ids.

use std::collections::HashMap;

use hz_core::anim::Animation;
use hz_core::util::Reader;
use hz_core::{kind, Pack};

#[derive(Clone, Debug)]
pub struct State {
    pub rot_x: u8,
    pub rot_y: u8,
    pub uvlock: bool,
    pub weight: u8,
    pub props: Vec<(String, String)>,
    pub model: u16,
}

#[derive(Clone, Debug)]
pub struct BlockDef {
    pub name: String,
    pub states: Vec<State>,
}

#[derive(Clone, Debug)]
pub struct Quad {
    pub face: u8,
    pub cullface: u8,
    pub tint: i16,
    pub rotation: u8,
    /// Index into `Registry::texture_ids`, or `u16::MAX` for "no texture".
    pub texture: u16,
    /// 4 corners, block-local 0..1.
    pub pos: [[f32; 3]; 4],
    /// 4 (u, v) pairs.
    pub uv: [[f32; 2]; 4],
}

#[derive(Clone, Debug, Default)]
pub struct Model {
    pub textures: Vec<(String, String)>,
    pub quads: Vec<Quad>,
}

pub struct Registry {
    pub blocks: Vec<BlockDef>,
    pub models: Vec<Model>,
    /// Global texture ids in first-seen order: `["minecraft:block/stone", ...]`.
    /// The renderer uploads one array slice per entry.
    pub texture_ids: Vec<String>,
    /// PNG bytes matching `texture_ids`, empty when the pack had no texture.
    pub texture_pngs: Vec<Vec<u8>>,
    /// Sound events and their payloads.
    pub sounds: crate::sound::SoundCatalog,
    /// Shader packs found in the pack: name, version, stages and what they need
    /// beyond the engine. The renderer does not execute them yet (see
    /// docs/SHADERS.md); they are listed so the app can be honest about it.
    pub shaders: Vec<crate::shader::ShaderPackInfo>,
    /// Animations keyed by texture slot.
    pub animations: std::collections::HashMap<u16, Animation>,
    /// Dense per-block tables: light blocking and emission. Both are consulted
    /// several times per voxel while meshing and relighting, so they are arrays
    /// rather than name lookups.
    opacity: Vec<u8>,
    emission: Vec<u8>,
    tex_index: HashMap<String, u16>,
    by_name: HashMap<String, u16>,
    /// Air is always id 0.
    pub air: u16,
    pub stone: u16,
}

fn parse_model(bin: &[u8]) -> Result<Model, String> {
    let mut r = Reader::new(bin);
    let magic = r.take(3)?;
    if magic != b"MD2" {
        return Err(format!(
            "model: bad magic {:?} (this engine reads MD2)",
            String::from_utf8_lossy(magic)
        ));
    }
    let ntex = r.u16()? as usize;
    let mut textures = Vec::with_capacity(ntex);
    for _ in 0..ntex {
        let k = r.str16()?;
        let v = r.str16()?;
        textures.push((k, v));
    }
    let nq = r.u16()? as usize;
    let mut quads = Vec::with_capacity(nq);
    for _ in 0..nq {
        let face = r.u8()?;
        let cullface = r.u8()?;
        let tint = {
            let b = r.take(2)?;
            i16::from_le_bytes([b[0], b[1]])
        };
        let rotation = r.u8()?;
        let texture = r.u16()?;
        let mut pos = [[0.0f32; 3]; 4];
        for p in pos.iter_mut() {
            p[0] = r.f32()?;
            p[1] = r.f32()?;
            p[2] = r.f32()?;
        }
        let mut uv = [[0.0f32; 2]; 4];
        for u in uv.iter_mut() {
            u[0] = r.f32()?;
            u[1] = r.f32()?;
        }
        quads.push(Quad {
            face,
            cullface,
            tint,
            rotation,
            texture,
            pos,
            uv,
        });
    }
    Ok(Model { textures, quads })
}

fn parse_blockstates(bin: &[u8]) -> Result<Vec<State>, String> {
    let mut r = Reader::new(bin);
    if r.take(3)? != b"BS1" {
        return Err("blockstates: bad magic".to_string());
    }
    let n = r.u32()? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let rot_x = r.u8()?;
        let rot_y = r.u8()?;
        let flags = r.u8()?;
        let weight = r.u8()?;
        let pc = r.u16()? as usize;
        let mut props = Vec::with_capacity(pc);
        for _ in 0..pc {
            let k = r.str16()?;
            let v = r.str16()?;
            props.push((k, v));
        }
        let model_name = r.str16()?;
        out.push(State {
            rot_x,
            rot_y,
            uvlock: flags & 1 != 0,
            weight,
            props,
            model: 0xFFFF,
        });
        out.last_mut()
            .unwrap()
            .props
            .push(("$model".into(), model_name));
    }
    Ok(out)
}

impl Registry {
    pub fn build(pack: &Pack) -> Result<Registry, String> {
        let mut models = Vec::new();
        let mut model_ids: HashMap<String, u16> = HashMap::new();
        let mut texture_ids: Vec<String> = Vec::new();
        let mut tex_index: HashMap<String, u16> = HashMap::new();
        for e in &pack.entries {
            if e.kind != kind::MODEL {
                continue;
            }
            let id = e
                .name
                .strip_prefix("models/")
                .ok_or_else(|| format!("model entry has odd name {}", e.name))?;
            let mut m = parse_model(&e.data)?;
            // Resolve per-model slots into the global table.
            for q in m.quads.iter_mut() {
                if q.texture == u16::MAX {
                    continue;
                }
                let id = match m.textures.get(q.texture as usize) {
                    Some((_, resolved)) => resolved.clone(),
                    None => continue,
                };
                q.texture = match tex_index.get(&id) {
                    Some(existing) => *existing,
                    None => {
                        let next = texture_ids.len() as u16;
                        texture_ids.push(id.clone());
                        tex_index.insert(id, next);
                        next
                    }
                };
            }
            model_ids.insert(id.to_string(), models.len() as u16);
            models.push(m);
        }

        let mut blocks: Vec<BlockDef> = Vec::new();
        let mut by_name: HashMap<String, u16> = HashMap::new();
        for e in &pack.entries {
            if e.kind != kind::BLOCKSTATES {
                continue;
            }
            let name = e
                .name
                .strip_prefix("blockstates/")
                .unwrap_or(&e.name)
                .to_string();
            let mut states = parse_blockstates(&e.data)?;
            for s in states.iter_mut() {
                let model_name = s
                    .props
                    .iter()
                    .position(|(k, _)| k == "$model")
                    .map(|i| s.props.remove(i).1)
                    .unwrap_or_else(|| "minecraft:block/air".to_string());
                s.model = *model_ids.get(&model_name).unwrap_or(&0);
            }
            by_name.insert(name.clone(), blocks.len() as u16);
            blocks.push(BlockDef { name, states });
        }

        // Air must be id 0: it is the dense default the chunk palette and the
        // renderer rely on. Reorder once, then rebuild the name index.
        by_name.clear();
        match blocks.iter().position(|b| b.name == "minecraft:air") {
            Some(0) => {}
            Some(i) => blocks.swap(0, i),
            None => blocks.insert(
                0,
                BlockDef {
                    name: "minecraft:air".to_string(),
                    states: Vec::new(),
                },
            ),
        }
        for (i, b) in blocks.iter().enumerate() {
            by_name.insert(b.name.clone(), i as u16);
        }
        let air = 0u16;
        let stone = by_name
            .get("minecraft:stone")
            .copied()
            .unwrap_or(if blocks.len() > 1 { 1 } else { 0 });
        // Attach the PNG payloads; a texture referenced by a model but absent
        // from the pack keeps an empty slot, which the renderer shows as colour.
        let mut texture_pngs: Vec<Vec<u8>> = vec![Vec::new(); texture_ids.len()];
        for e in &pack.entries {
            if e.kind != kind::TEXTURE {
                continue;
            }
            if let Some(id) = e.name.strip_prefix("textures/") {
                if let Some(slot) = tex_index.get(id) {
                    texture_pngs[*slot as usize] = e.data.clone();
                }
            }
        }

        let mut animations: std::collections::HashMap<u16, Animation> =
            std::collections::HashMap::new();
        for e in &pack.entries {
            if e.kind != kind::ANIM {
                continue;
            }
            let id = match e.name.strip_prefix("anim/") {
                Some(id) => id,
                None => continue,
            };
            let slot = match tex_index.get(id) {
                Some(slot) => *slot,
                None => continue, // animation for a texture no model references
            };
            match Animation::from_bytes(&e.data) {
                Ok(anim) if anim.frame_count() > 1 => {
                    animations.insert(slot, anim);
                }
                Ok(_) => {}
                Err(_) => continue,
            }
        }

        let mut opacity = Vec::with_capacity(blocks.len());
        let mut emission = Vec::with_capacity(blocks.len());
        for (index, b) in blocks.iter().enumerate() {
            let emit = Self::emission_by_name(&b.name);
            // Air never blocks — it is id 0 by construction, and the name table
            // alone would classify it as solid because it matches no entry.
            // Emitters are props, not full blocks: a torch must not swallow the
            // light it produces.
            let blocks_light = index != 0 && emit == 0 && Self::opaque_by_name(&b.name);
            opacity.push(u8::from(blocks_light));
            emission.push(emit);
        }

        let sound_entries: Vec<(u8, String, Vec<u8>)> = pack
            .entries
            .iter()
            .filter(|e| e.kind == kind::SOUND)
            .map(|e| (e.kind, e.name.clone(), e.data.clone()))
            .collect();
        // A bad catalog must not make the world unloadable: the game stays
        // playable, just silent, and the app reports the missing audio.
        let sounds = crate::sound::SoundCatalog::build(&sound_entries).unwrap_or_default();

        let shaders = crate::shader::collect(pack);

        Ok(Registry {
            blocks,
            models,
            sounds,
            shaders,
            texture_ids,
            texture_pngs,
            animations,
            opacity,
            emission,
            tex_index,
            by_name,
            air,
            stone,
        })
    }

    pub fn id(&self, name: &str) -> Option<u16> {
        self.by_name.get(name).copied()
    }

    pub fn name(&self, id: u16) -> &str {
        self.blocks
            .get(id as usize)
            .map(|b| b.name.as_str())
            .unwrap_or("minecraft:air")
    }

    pub fn state(&self, block: u16) -> Option<&State> {
        self.blocks.get(block as usize)?.states.first()
    }

    pub fn model_for(&self, block: u16, meta: u8) -> Option<&Model> {
        let def = self.blocks.get(block as usize)?;
        if def.states.is_empty() {
            return None;
        }
        let idx = (meta as usize).min(def.states.len() - 1);
        self.models.get(def.states[idx].model as usize)
    }

    /// Animations that actually have more than one frame.
    pub fn animation_count(&self) -> usize {
        self.animations.len()
    }

    /// Shader packs whose interface the engine could drive as-is.
    pub fn supported_shader_count(&self) -> usize {
        self.shaders.iter().filter(|s| s.supported).count()
    }

    pub fn texture_count(&self) -> usize {
        self.texture_ids.len()
    }

    pub fn texture_png(&self, index: u16) -> &[u8] {
        self.texture_pngs
            .get(index as usize)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Block light emitted by `id`, 0..=15.
    ///
    /// Vanilla keeps this on the Java class, not in the jar's data, so until the
    /// transpile tier supplies real values this is a name table covering the
    /// blocks players actually build with. Names are matched on the path part,
    /// so `minecraft:torch` and a resource pack's `torch` both hit.
    pub fn light_emission(&self, id: u16) -> u8 {
        self.emission.get(id as usize).copied().unwrap_or(0)
    }

    /// Emission for a block name; used once at load time to fill the table.
    fn emission_by_name(name: &str) -> u8 {
        let path = name.split(':').next_back().unwrap_or(name);
        match path {
            "torch" | "wall_torch" | "soul_torch" | "soul_wall_torch" => 14,
            "lantern" | "soul_lantern" | "end_rod" => 15,
            "glowstone" | "shroomlight" | "jack_o_lantern" | "sea_lantern" | "beacon" => 15,
            "redstone_lamp" | "campfire" | "soul_campfire" => 15,
            "lava" | "flowing_lava" | "magma_block" | "fire" | "soul_fire" => 15,
            "respawn_anchor"
            | "glow_lichen"
            | "ochre_froglight"
            | "verdant_froglight"
            | "pearlescent_froglight" => 15,
            "crying_obsidian" => 10,
            "amethyst_cluster" | "large_amethyst_bud" => 5,
            "amethyst_block" | "budding_amethyst" => 1,
            "brewing_stand" | "enchanting_table" | "ender_chest" | "crying_obsidian_ore" => 7,
            "brown_mushroom" => 1,
            "sculk_catalyst" | "sculk_sensor" | "sculk_shrieker" => 6,
            _ => 0,
        }
    }

    /// Whether light stops here. Glass, water and the other see-through blocks
    /// transmit, which is what makes windows and underwater spaces readable.
    pub fn is_opaque(&self, id: u16) -> bool {
        self.opacity.get(id as usize).copied().unwrap_or(1) != 0
    }

    /// Opacity for a block name; used once at load time to fill the table.
    fn opaque_by_name(name: &str) -> bool {
        let path = name.split(':').next_back().unwrap_or(name);
        !matches!(
            path,
            "glass"
                | "glass_pane"
                | "tinted_glass"
                | "water"
                | "flowing_water"
                | "ice"
                | "frosted_ice"
                | "blue_ice"
                | "tripwire"
                | "light"
                | "barrier"
                | "structure_void"
        )
    }

    pub fn texture_slot(&self, texture_id: &str) -> Option<u16> {
        self.tex_index.get(texture_id).copied()
    }

    pub fn quad_count(&self) -> usize {
        self.models.iter().map(|m| m.quads.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hz_core::Buf;

    fn bs(state_count: u32, model: &str) -> Vec<u8> {
        let mut b = Buf::new();
        b.bytes(b"BS1");
        b.u32(state_count);
        for _ in 0..state_count {
            b.u8(0);
            b.u8(0);
            b.u8(0);
            b.u8(1);
            b.u16(0);
            b.str16(model);
        }
        b.finish()
    }

    fn md(quads: usize) -> Vec<u8> {
        let mut b = Buf::new();
        b.bytes(b"MD2");
        b.u16(1);
        b.str16("all");
        b.str16("minecraft:block/stone");
        b.u16(quads as u16);
        for _ in 0..quads {
            b.u8(0);
            b.u8(0);
            b.i16(-1);
            b.u8(0);
            b.u16(0);
            for c in 0..4 {
                b.f32(c as f32);
                b.f32(0.0);
                b.f32(0.0);
            }
            for _ in 0..4 {
                b.f32(0.0);
                b.f32(0.0);
            }
        }
        b.finish()
    }

    #[test]
    fn global_texture_table_merges_and_dedupes() {
        // two models, three texture references, two distinct ids
        let mut b = Buf::new();
        b.bytes(b"MD2");
        b.u16(2);
        b.str16("all");
        b.str16("minecraft:block/stone");
        b.str16("side");
        b.str16("minecraft:block/dirt");
        b.u16(2);
        for tex in [0u16, 1u16] {
            b.u8(0);
            b.u8(0);
            b.i16(-1);
            b.u8(0);
            b.u16(tex);
            for c in 0..4 {
                b.f32(c as f32);
                b.f32(0.0);
                b.f32(0.0);
            }
            for _ in 0..4 {
                b.f32(0.0);
                b.f32(0.0);
            }
        }

        let mut second = Buf::new();
        second.bytes(b"MD2");
        second.u16(1);
        second.str16("all");
        second.str16("minecraft:block/stone"); // duplicate of model A
        second.u16(1);
        second.u8(0);
        second.u8(0);
        second.i16(-1);
        second.u8(0);
        second.u16(0);
        for c in 0..4 {
            second.f32(c as f32);
            second.f32(0.0);
            second.f32(0.0);
        }
        for _ in 0..4 {
            second.f32(0.0);
            second.f32(0.0);
        }

        let mut p = Pack::new();
        p.add(kind::MODEL, "models/minecraft:block/a", b.finish());
        p.add(kind::MODEL, "models/minecraft:block/b", second.finish());
        p.add(
            kind::TEXTURE,
            "textures/minecraft:block/stone",
            vec![1, 2, 3],
        );
        p.add(
            kind::BLOCKSTATES,
            "blockstates/minecraft:stone",
            bs(1, "minecraft:block/a"),
        );

        let reg = Registry::build(&p).unwrap();
        assert_eq!(reg.texture_ids.len(), 2, "stone and dirt, stone deduped");
        assert_eq!(reg.texture_ids[0], "minecraft:block/stone");
        assert_eq!(reg.texture_ids[1], "minecraft:block/dirt");
        // model A: quad 0 -> stone (0), quad 1 -> dirt (1); model B -> stone (0)
        assert_eq!(reg.models[0].quads[0].texture, 0);
        assert_eq!(reg.models[0].quads[1].texture, 1);
        assert_eq!(reg.models[1].quads[0].texture, 0);
        // the PNG came through
        assert_eq!(reg.texture_png(0), &[1, 2, 3]);
        assert_eq!(reg.texture_png(1), &[] as &[u8]);
    }

    #[test]
    fn builds_from_pack() {
        let mut p = Pack::new();
        p.add(kind::MODEL, "models/minecraft:block/cube", md(2));
        p.add(
            kind::BLOCKSTATES,
            "blockstates/minecraft:stone",
            bs(1, "minecraft:block/cube"),
        );
        p.add(
            kind::BLOCKSTATES,
            "blockstates/minecraft:air",
            bs(1, "minecraft:block/cube"),
        );
        let reg = Registry::build(&p).unwrap();
        assert_eq!(reg.blocks.len(), 2);
        assert_eq!(reg.models.len(), 1);
        assert_eq!(reg.models[0].quads.len(), 2);
        assert_eq!(reg.id("minecraft:stone"), Some(reg.stone));
        assert_eq!(reg.name(reg.air), "minecraft:air");
        assert_eq!(reg.quad_count(), 2);
        assert_eq!(reg.texture_ids, vec!["minecraft:block/stone".to_string()]);
        assert_eq!(reg.texture_slot("minecraft:block/stone"), Some(0));
        assert_eq!(reg.models[0].quads[0].texture, 0);
    }
}
