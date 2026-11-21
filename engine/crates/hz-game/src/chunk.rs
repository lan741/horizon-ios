//! 16³ palette-compressed section.
//!
//! Same idea as the vanilla format, but u16 block ids and no lighting: the
//! renderer computes light from the sky/occlusion grid each frame. Palette
//! entries grow from 4 to 8 to 16 bits, then fall back to a dense array.

pub const SIZE: usize = 16;
pub const VOLUME: usize = SIZE * SIZE * SIZE;

#[derive(Clone)]
pub struct Chunk {
    palette: Vec<u16>,
    bits: u8,
    data: Vec<u64>,
    /// Dense fallback when a section holds more than 256 distinct blocks.
    dense: Option<Vec<u16>>,
    pub non_air: u32,
}

fn idx(x: usize, y: usize, z: usize) -> usize {
    (y * SIZE + z) * SIZE + x
}

impl Chunk {
    pub fn new(fill: u16) -> Chunk {
        let mut c = Chunk {
            palette: Vec::new(),
            bits: 4,
            data: Vec::new(),
            dense: None,
            non_air: 0,
        };
        c.palette.push(fill);
        c.palette.push(0);
        c.reset_words();
        if fill != 0 {
            for i in 0..VOLUME {
                c.set_index(i, fill);
            }
        }
        c.recount(fill);
        c
    }

    fn reset_words(&mut self) {
        let per_word = 64 / self.bits as usize;
        let words = VOLUME.div_ceil(per_word);
        self.data = vec![0u64; words];
    }

    fn recount(&mut self, _fill: u16) {
        let mut n = 0u32;
        for i in 0..VOLUME {
            if self.get_index(i) != 0 {
                n += 1;
            }
        }
        self.non_air = n;
    }

    fn set_index(&mut self, i: usize, id: u16) {
        if let Some(d) = self.dense.as_mut() {
            d[i] = id;
            return;
        }
        let pi = self.palette_index(id);
        if pi != usize::MAX {
            self.put_bits(i, pi as u64);
        }
    }

    fn palette_index(&mut self, id: u16) -> usize {
        if let Some(p) = self.palette.iter().position(|&v| v == id) {
            return p;
        }
        if self.palette.len() < (1usize << self.bits) {
            self.palette.push(id);
            return self.palette.len() - 1;
        }
        if self.bits < 16 {
            self.grow();
            return self.palette_index(id);
        }
        // 16 bits already: go dense
        self.densify();
        usize::MAX
    }

    fn grow(&mut self) {
        let old: Vec<u16> = (0..VOLUME).map(|i| self.get_index(i)).collect();
        self.bits *= 2;
        self.reset_words();
        for (i, v) in old.iter().enumerate() {
            let pi = self.palette.iter().position(|&x| x == *v).unwrap_or(0) as u64;
            self.put_bits(i, pi);
        }
    }

    fn densify(&mut self) {
        let old: Vec<u16> = (0..VOLUME).map(|i| self.get_index(i)).collect();
        self.dense = Some(old);
        self.data.clear();
    }

    fn put_bits(&mut self, i: usize, v: u64) {
        let per_word = 64 / self.bits as usize;
        let w = i / per_word;
        let shift = (i % per_word) * self.bits as usize;
        let mask = if self.bits == 64 {
            u64::MAX
        } else {
            (1u64 << self.bits) - 1
        };
        self.data[w] = (self.data[w] & !(mask << shift)) | ((v & mask) << shift);
    }

    fn get_index(&self, i: usize) -> u16 {
        if let Some(d) = &self.dense {
            return d[i];
        }
        if self.data.is_empty() {
            return *self.palette.first().unwrap_or(&0);
        }
        let per_word = 64 / self.bits as usize;
        let w = i / per_word;
        let shift = (i % per_word) * self.bits as usize;
        let mask = if self.bits == 64 {
            u64::MAX
        } else {
            (1u64 << self.bits) - 1
        };
        let pi = ((self.data[w] >> shift) & mask) as usize;
        *self.palette.get(pi).unwrap_or(&0)
    }

    pub fn get(&self, x: usize, y: usize, z: usize) -> u16 {
        self.get_index(idx(x, y, z))
    }

    pub fn set(&mut self, x: usize, y: usize, z: usize, id: u16) {
        let i = idx(x, y, z);
        let old = self.get_index(i);
        if old == id {
            return;
        }
        self.set_index(i, id);
        if old == 0 && id != 0 {
            self.non_air += 1;
        } else if old != 0 && id == 0 {
            self.non_air = self.non_air.saturating_sub(1);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.non_air == 0
    }

    pub fn palette_len(&self) -> usize {
        self.palette.len()
    }

    pub fn bits(&self) -> u8 {
        self.bits
    }

    pub fn palette_snapshot(&self) -> Vec<u16> {
        self.palette.clone()
    }

    pub fn words_snapshot(&self) -> Vec<u64> {
        self.data.clone()
    }

    pub fn dense_snapshot(&self) -> Option<Vec<u16>> {
        self.dense.clone()
    }

    /// Rebuild a section from the on-disk parts. `words` must be sized for
    /// `bits`; a mismatch is an error rather than a silently empty section.
    pub fn from_parts(bits: u8, palette: Vec<u16>, words: Vec<u64>) -> Result<Chunk, String> {
        if palette.is_empty() {
            return Err("chunk: empty palette".to_string());
        }
        if palette.len() > (1usize << bits) {
            return Err(format!(
                "chunk: palette {} larger than {} bits allow",
                palette.len(),
                bits
            ));
        }
        let per_word = 64 / bits as usize;
        let want = VOLUME.div_ceil(per_word);
        if words.len() != want {
            return Err(format!(
                "chunk: {} words for {} bits, want {}",
                words.len(),
                bits,
                want
            ));
        }
        let mut c = Chunk {
            palette,
            bits,
            data: words,
            dense: None,
            non_air: 0,
        };
        let mut n = 0u32;
        for i in 0..VOLUME {
            if c.get_index(i) != 0 {
                n += 1;
            }
        }
        c.non_air = n;
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_across_palette_growth() {
        let mut c = Chunk::new(0);
        for i in 0..240u16 {
            let x = (i as usize) % 16;
            let z = (i as usize / 16) % 15;
            c.set(x, 0, z, i + 1);
        }
        for i in 0..240u16 {
            let x = (i as usize) % 16;
            let z = (i as usize / 16) % 15;
            assert_eq!(c.get(x, 0, z), i + 1);
        }
        assert!(c.bits() >= 4);
        assert!(c.non_air >= 39);
    }

    #[test]
    fn dense_fallback_keeps_values() {
        let mut c = Chunk::new(1);
        for i in 0..VOLUME {
            c.set(i % 16, i / 16 % 16, i / 256, (i % 300) as u16 + 1);
        }
        for i in 0..VOLUME {
            assert_eq!(c.get(i % 16, i / 16 % 16, i / 256), (i % 300) as u16 + 1);
        }
    }
}
