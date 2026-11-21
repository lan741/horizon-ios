//! `.hzp` — HorizonPad's native pack container.
//!
//! One file, no ZIP, no DEFLATE: the iPad maps it read-only and the engine
//! indexes entries by name without parsing JSON at startup.
//!
//! Layout:
//! ```text
//! "HZP1" | u16 version | u16 reserved | u32 count | u32 index_len
//! index[count]:
//!   u8 kind | u16 name_len | name | 32-byte sha256 | u32 offset | u32 length
//! blob region (offsets are relative to blob start)
//! ```
//! Strings are length-prefixed UTF-8, integers little-endian.

use crate::kind_name;
use crate::sha;
use crate::util::{Buf, Reader};

pub struct PackEntry {
    pub kind: u8,
    pub name: String,
    pub sha256: [u8; 32],
    pub data: Vec<u8>,
}

pub struct Pack {
    pub version: u16,
    pub entries: Vec<PackEntry>,
}

pub const VERSION: u16 = 1;

impl Pack {
    pub fn new() -> Pack {
        Pack {
            version: VERSION,
            entries: Vec::new(),
        }
    }

    pub fn add(&mut self, kind: u8, name: impl Into<String>, data: Vec<u8>) {
        let name = name.into();
        let sha256 = sha::sha256(&data);
        self.entries.push(PackEntry {
            kind,
            name,
            sha256,
            data,
        });
    }

    pub fn get(&self, name: &str) -> Option<&PackEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    pub fn count_kind(&self, k: u8) -> usize {
        self.entries.iter().filter(|e| e.kind == k).count()
    }

    pub fn write(&self) -> Vec<u8> {
        let mut idx = Buf::with_capacity(1024);
        let mut count = 0u32;
        let mut offsets: Vec<(usize, usize)> = Vec::with_capacity(self.entries.len());
        let mut blob_len = 0usize;
        for e in &self.entries {
            offsets.push((blob_len, e.data.len()));
            blob_len += e.data.len();
        }
        for (e, (off, len)) in self.entries.iter().zip(offsets.iter()) {
            idx.u8(e.kind);
            idx.str16(&e.name);
            idx.bytes(&e.sha256);
            idx.u32(*off as u32);
            idx.u32(*len as u32);
            count += 1;
        }

        let mut out = Buf::with_capacity(idx.d.len() + blob_len + 16);
        out.bytes(b"HZP1");
        out.u16(self.version);
        out.u16(0);
        out.u32(count);
        out.u32(idx.d.len() as u32);
        out.bytes(&idx.d);
        for e in &self.entries {
            out.bytes(&e.data);
        }
        out.finish()
    }

    pub fn read(d: &[u8]) -> Result<Pack, String> {
        let mut r = Reader::new(d);
        let magic = r.take(4)?;
        if magic != b"HZP1" {
            return Err("pack: bad magic".to_string());
        }
        let version = r.u16()?;
        let _reserved = r.u16()?;
        let count = r.u32()? as usize;
        let index_len = r.u32()? as usize;
        if count > 8_000_000 {
            return Err("pack: implausible entry count".to_string());
        }
        let index_bytes = r.take(index_len)?.to_vec();
        let blob_off = r.i;
        let mut ir = Reader::new(&index_bytes);
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let kind = ir.u8()?;
            let name = ir.str16()?;
            let mut sha256 = [0u8; 32];
            sha256.copy_from_slice(ir.take(32)?);
            let off = ir.u32()? as usize;
            let len = ir.u32()? as usize;
            if blob_off + off + len > d.len() {
                return Err(format!("pack: entry {} outside file", name));
            }
            let data = d[blob_off + off..blob_off + off + len].to_vec();
            entries.push(PackEntry {
                kind,
                name,
                sha256,
                data,
            });
        }
        Ok(Pack { version, entries })
    }

    /// Human-readable summary: `{ "blockstates": 1200, ... }`.
    pub fn summary_json(&self) -> String {
        let mut kinds: Vec<(u8, usize)> = Vec::new();
        for e in &self.entries {
            match kinds.iter_mut().find(|(k, _)| *k == e.kind) {
                Some((_, c)) => *c += 1,
                None => kinds.push((e.kind, 1)),
            }
        }
        kinds.sort_by_key(|(k, _)| *k);
        let mut s = String::from("{");
        for (i, (k, c)) in kinds.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!("\"{}\":{}", kind_name(*k), c));
        }
        s.push('}');
        s
    }
}

impl Default for Pack {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind;

    #[test]
    fn roundtrip() {
        let mut p = Pack::new();
        p.add(kind::LANG, "en_us", b"a=b".to_vec());
        p.add(kind::DATA, "recipes/x.json", b"{}".to_vec());
        let bytes = p.write();
        let back = Pack::read(&bytes).unwrap();
        assert_eq!(back.entries.len(), 2);
        assert_eq!(back.get("en_us").unwrap().data, b"a=b");
        assert_eq!(back.count_kind(kind::LANG), 1);
        assert_eq!(back.summary_json(), "{\"lang\":1,\"data\":1}");
    }
}
