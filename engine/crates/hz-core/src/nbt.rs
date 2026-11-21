//! Named Binary Tag (Minecraft's NBT), plus the gzip wrapper region files use.
//!
//! Only what the port pipeline and the world importer need: full read/write
//! for every tag type, and gzip decompression built on our own inflate.

use crate::inflate::inflate;

#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    End,
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List(Vec<Tag>),
    Compound(Vec<(String, Tag)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Tag {
    pub fn get(&self, key: &str) -> Option<&Tag> {
        match self {
            Tag::Compound(v) => v.iter().find(|(k, _)| k == key).map(|(_, t)| t),
            _ => None,
        }
    }

    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Tag::Byte(v) => Some(*v as i32),
            Tag::Short(v) => Some(*v as i32),
            Tag::Int(v) => Some(*v),
            Tag::Long(v) => Some(*v as i32),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Tag::String(s) => Some(s),
            _ => None,
        }
    }
}

struct Cur<'a> {
    d: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    fn u8(&mut self) -> Result<u8, String> {
        let b = *self.d.get(self.i).ok_or("nbt: truncated")?;
        self.i += 1;
        Ok(b)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.i + n > self.d.len() {
            return Err("nbt: truncated payload".to_string());
        }
        let s = &self.d[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_be_bytes(a))
    }
    fn string(&mut self) -> Result<String, String> {
        let n = self.u16()? as usize;
        let b = self.take(n)?;
        Ok(String::from_utf8_lossy(b).into_owned())
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
}

pub fn read_nbt(buf: &[u8]) -> Result<(String, Tag), String> {
    let mut c = Cur { d: buf, i: 0 };
    let ty = c.u8()?;
    if ty != 10 {
        return Err(format!("nbt: root tag is {} (want compound)", ty));
    }
    let name = c.string()?;
    let tag = read_payload(&mut c, ty, 0)?;
    Ok((name, tag))
}

fn read_payload(c: &mut Cur<'_>, ty: u8, depth: u32) -> Result<Tag, String> {
    if depth > 512 {
        return Err("nbt: nesting too deep".to_string());
    }
    Ok(match ty {
        0 => Tag::End,
        1 => Tag::Byte(c.u8()? as i8),
        2 => Tag::Short(c.u16()? as i16),
        3 => Tag::Int(c.i32()?),
        4 => Tag::Long(c.u64()? as i64),
        5 => Tag::Float(f32::from_bits(c.u32()?)),
        6 => Tag::Double(f64::from_bits(c.u64()?)),
        7 => {
            let n = c.i32()? as usize;
            Tag::ByteArray(c.take(n)?.to_vec())
        }
        8 => Tag::String(c.string()?),
        9 => {
            let et = c.u8()?;
            let n = c.i32()? as usize;
            if n > 64 * 1024 * 1024 {
                return Err("nbt: list length implausible".to_string());
            }
            let mut v = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                v.push(read_payload(c, et, depth + 1)?);
            }
            Tag::List(v)
        }
        10 => {
            let mut v = Vec::new();
            loop {
                let t = c.u8()?;
                if t == 0 {
                    break;
                }
                let k = c.string()?;
                v.push((k, read_payload(c, t, depth + 1)?));
            }
            Tag::Compound(v)
        }
        11 => {
            let n = c.i32()? as usize;
            if n > 16 * 1024 * 1024 {
                return Err("nbt: int array length implausible".to_string());
            }
            let mut v = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                v.push(c.i32()?);
            }
            Tag::IntArray(v)
        }
        12 => {
            let n = c.i32()? as usize;
            if n > 16 * 1024 * 1024 {
                return Err("nbt: long array length implausible".to_string());
            }
            let mut v = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                v.push(c.u64()? as i64);
            }
            Tag::LongArray(v)
        }
        other => return Err(format!("nbt: unknown tag type {}", other)),
    })
}

pub fn write_nbt(name: &str, root: &Tag) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(10);
    put_str(&mut out, name);
    write_payload(&mut out, root);
    out
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn write_payload(out: &mut Vec<u8>, t: &Tag) {
    match t {
        Tag::End => {}
        Tag::Byte(v) => out.push(*v as u8),
        Tag::Short(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
        Tag::Float(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
        Tag::Double(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
        Tag::ByteArray(v) => {
            out.extend_from_slice(&(v.len() as i32).to_be_bytes());
            out.extend_from_slice(v);
        }
        Tag::String(s) => put_str(out, s),
        Tag::List(v) => {
            let et = match v.first() {
                None => 0,
                Some(Tag::Byte(_)) => 1,
                Some(Tag::Short(_)) => 2,
                Some(Tag::Int(_)) => 3,
                Some(Tag::Long(_)) => 4,
                Some(Tag::Float(_)) => 5,
                Some(Tag::Double(_)) => 6,
                Some(Tag::ByteArray(_)) => 7,
                Some(Tag::String(_)) => 8,
                Some(Tag::List(_)) => 9,
                Some(Tag::Compound(_)) => 10,
                Some(Tag::IntArray(_)) => 11,
                Some(Tag::LongArray(_)) => 12,
                Some(Tag::End) => 0,
            };
            out.push(et);
            out.extend_from_slice(&(v.len() as i32).to_be_bytes());
            for e in v {
                write_payload(out, e);
            }
        }
        Tag::Compound(v) => {
            for (k, child) in v {
                out.push(tag_id(child));
                put_str(out, k);
                write_payload(out, child);
            }
            out.push(0);
        }
        Tag::IntArray(v) => {
            out.extend_from_slice(&(v.len() as i32).to_be_bytes());
            for e in v {
                out.extend_from_slice(&e.to_be_bytes());
            }
        }
        Tag::LongArray(v) => {
            out.extend_from_slice(&(v.len() as i32).to_be_bytes());
            for e in v {
                out.extend_from_slice(&e.to_be_bytes());
            }
        }
    }
}

fn tag_id(t: &Tag) -> u8 {
    match t {
        Tag::End => 0,
        Tag::Byte(_) => 1,
        Tag::Short(_) => 2,
        Tag::Int(_) => 3,
        Tag::Long(_) => 4,
        Tag::Float(_) => 5,
        Tag::Double(_) => 6,
        Tag::ByteArray(_) => 7,
        Tag::String(_) => 8,
        Tag::List(_) => 9,
        Tag::Compound(_) => 10,
        Tag::IntArray(_) => 11,
        Tag::LongArray(_) => 12,
    }
}

/// Decompress a gzip member (`1f 8b 08`), which is how `level.dat` and
/// region payloads arrive.
pub fn gzip_decompress(d: &[u8]) -> Result<Vec<u8>, String> {
    if d.len() < 18 {
        return Err("gzip: too short".to_string());
    }
    if d[0] != 0x1f || d[1] != 0x8b || d[2] != 8 {
        return Err("gzip: not a gzip member".to_string());
    }
    let flags = d[3];
    let mut p = 10usize;
    if flags & 0x04 != 0 {
        if p + 2 > d.len() {
            return Err("gzip: truncated extra field".to_string());
        }
        let xlen = u16::from_le_bytes([d[p], d[p + 1]]) as usize;
        p += 2 + xlen;
    }
    if flags & 0x08 != 0 {
        while p < d.len() && d[p] != 0 {
            p += 1;
        }
        p += 1;
    }
    if flags & 0x10 != 0 {
        while p < d.len() && d[p] != 0 {
            p += 1;
        }
        p += 1;
    }
    if flags & 0x02 != 0 {
        p += 2;
    }
    if p + 8 > d.len() {
        return Err("gzip: truncated body".to_string());
    }
    let body = &d[p..d.len() - 8];
    inflate(body, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let root = Tag::Compound(vec![(
            "Data".into(),
            Tag::Compound(vec![
                ("LevelName".into(), Tag::String("Test".into())),
                ("Seed".into(), Tag::Long(-42)),
                (
                    "Sections".into(),
                    Tag::List(vec![Tag::Compound(vec![
                        ("Y".into(), Tag::Byte(0)),
                        ("Blocks".into(), Tag::ByteArray(vec![1, 2, 3])),
                    ])]),
                ),
            ]),
        )]);
        let bytes = write_nbt("", &root);
        let (name, back) = read_nbt(&bytes).unwrap();
        assert_eq!(name, "");
        assert_eq!(root, back);
    }
}
