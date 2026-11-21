//! World persistence: `.hzw` — HorizonPad's world file.
//!
//! ```text
//! "HZW1" | u16 version | u32 section_count
//! per section: i32 x | i32 y | i32 z
//!              u8 bits (0 = dense fallback) | u16 palette_len | u16 palette[]
//!              u32 word_count | u64 word[]
//! trailer: 3×f32 position | f32 yaw | f32 pitch | u64 ticks
//! ```
//! Chunks are stored exactly as the runtime holds them (palette + packed
//! words), so saving an edited world costs one memcpy per section.

use crate::chunk::{Chunk, VOLUME};
use crate::game::{Game, Player};
use crate::registry::Registry;
use hz_core::util::{Buf, Reader};

pub const VERSION: u16 = 1;

impl Chunk {
    /// `(bits, palette, words, dense)` — `bits == 0` means the dense array.
    pub fn export(&self) -> (u8, Vec<u16>, Vec<u64>, Vec<u16>) {
        let dense = self.dense_snapshot();
        match dense {
            Some(d) => (0, Vec::new(), Vec::new(), d),
            None => (
                self.bits(),
                self.palette_snapshot(),
                self.words_snapshot(),
                Vec::new(),
            ),
        }
    }

    pub fn import(
        bits: u8,
        palette: Vec<u16>,
        words: Vec<u64>,
        dense: Vec<u16>,
    ) -> Result<Chunk, String> {
        if bits == 0 {
            if dense.len() != VOLUME {
                return Err(format!(
                    "chunk: dense array has {} entries, want {}",
                    dense.len(),
                    VOLUME
                ));
            }
            let mut c = Chunk::new(0);
            for (i, v) in dense.iter().enumerate() {
                c.set(i % 16, (i / 16) % 16, i / 256, *v);
            }
            return Ok(c);
        }
        if bits != 4 && bits != 8 && bits != 16 {
            return Err(format!("chunk: unsupported palette width {}", bits));
        }
        Chunk::from_parts(bits, palette, words)
    }
}

impl Game {
    pub fn save(&self, path: &str) -> Result<(), String> {
        let mut b = Buf::with_capacity(4096);
        b.bytes(b"HZW1");
        b.u16(VERSION);
        let sections: Vec<_> = self.world.sections().collect();
        b.u32(sections.len() as u32);
        for (&(x, y, z), chunk) in sections {
            b.i32(x);
            b.i32(y);
            b.i32(z);
            let (bits, palette, words, dense) = chunk.export();
            b.u8(bits);
            b.u16(palette.len() as u16);
            for p in &palette {
                b.u16(*p);
            }
            b.u32(words.len() as u32);
            for w in &words {
                b.u64(*w);
            }
            b.u32(dense.len() as u32);
            for d in &dense {
                b.u16(*d);
            }
        }
        let p = &self.player;
        b.f32(p.pos[0]);
        b.f32(p.pos[1]);
        b.f32(p.pos[2]);
        b.f32(p.yaw);
        b.f32(p.pitch);
        b.u64(self.ticks);
        std::fs::write(path, b.finish()).map_err(|e| format!("cannot write {}: {}", path, e))
    }

    pub fn load(&mut self, path: &str) -> Result<(), String> {
        let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {}", path, e))?;
        let mut r = Reader::new(&data);
        if r.take(4)? != b"HZW1" {
            return Err("world: bad magic".to_string());
        }
        let version = r.u16()?;
        if version != VERSION {
            return Err(format!("world: version {} is not {}", version, VERSION));
        }
        let n = r.u32()? as usize;
        let mut sections = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            let x = r.u32()? as i32;
            let y = r.u32()? as i32;
            let z = r.u32()? as i32;
            let bits = r.u8()?;
            let plen = r.u16()? as usize;
            let mut palette = Vec::with_capacity(plen);
            for _ in 0..plen {
                palette.push(r.u16()?);
            }
            let wlen = r.u32()? as usize;
            let mut words = Vec::with_capacity(wlen.min(1 << 20));
            for _ in 0..wlen {
                words.push(r.u64()?);
            }
            let dlen = r.u32()? as usize;
            let mut dense = Vec::with_capacity(dlen.min(VOLUME));
            for _ in 0..dlen {
                dense.push(r.u16()?);
            }
            sections.push(((x, y, z), Chunk::import(bits, palette, words, dense)?));
        }
        let pos = [r.f32()?, r.f32()?, r.f32()?];
        let yaw = r.f32()?;
        let pitch = r.f32()?;
        let ticks = r.u64()?;

        self.world.clear();
        for (key, chunk) in sections {
            self.world.insert(key, chunk);
        }
        self.player = Player {
            pos,
            vel: [0.0; 3],
            yaw,
            pitch,
            on_ground: false,
            flying: false,
        };
        self.ticks = ticks;
        Ok(())
    }

    /// Total blocks the world file will contain, for the save-progress UI.
    pub fn saved_block_count(&self) -> u32 {
        self.world.non_air()
    }
}

/// Rebind a freshly loaded world's ids to a registry: saves are only valid for
/// the pack they were made with, so the app checks the manifest hash first.
pub fn compatible(registry: &Registry, ids: &[u16]) -> bool {
    ids.iter()
        .all(|id| (*id as usize) < registry.blocks.len() || *id == registry.air)
}
#[cfg(test)]
mod tests {
    use super::*;
    use hz_core::{kind, Buf, Pack};

    fn pack_bytes() -> Vec<u8> {
        let mut md = Buf::new();
        md.bytes(b"MD2");
        md.u16(1);
        md.str16("all");
        md.str16("minecraft:block/stone");
        md.u16(1);
        md.u8(0);
        md.u8(1);
        md.i16(-1);
        md.u8(0);
        md.u16(0);
        for _ in 0..4 {
            md.f32(0.0);
            md.f32(0.0);
            md.f32(0.0);
        }
        for _ in 0..4 {
            md.f32(0.0);
            md.f32(0.0);
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
    fn save_load_roundtrip() {
        let mut g = Game::new(&pack_bytes()).unwrap();
        let stone = g.registry.stone;
        g.world.flat_pad(stone, 4);
        g.world.set(7, 9, -3, stone);
        g.player.pos = [1.5, 3.0, 2.5];
        g.player.yaw = 0.75;
        g.ticks = 4242;

        let dir = std::env::temp_dir().join("hz-world-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.hzw");
        let path_s = path.to_string_lossy().to_string();
        g.save(&path_s).unwrap();

        let before = g.world.non_air();
        let mut h = Game::new(&pack_bytes()).unwrap();
        h.load(&path_s).unwrap();
        assert_eq!(h.world.non_air(), before);
        assert_eq!(h.world.get(7, 9, -3), stone);
        assert_eq!(h.player.pos, [1.5, 3.0, 2.5]);
        assert_eq!(h.ticks, 4242);
        assert_eq!(h.world.get(0, 0, 0), stone);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut g = Game::new(&pack_bytes()).unwrap();
        let dir = std::env::temp_dir().join("hz-world-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.hzw");
        std::fs::write(&path, b"NOPE").unwrap();
        assert!(g.load(&path.to_string_lossy()).is_err());
    }
}
