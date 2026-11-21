//! hz-class — a JVM `.class` reader for the transpile report.
//!
//! Why this exists: the on-device pipeline turns Minecraft's *data* into native
//! descriptors, but the game's *behaviour* lives in ~20k Java classes. To say
//! honestly what remains, and to give the Mac/CI transpile tier something
//! exact to work against, we have to read the bytecode: constant pool, methods,
//! `Code` attribute, and the `invoke*` targets that become the Rust call graph.
//!
//! What it is not: a JVM. There is no execution, no verification, no
//! classloader semantics, no `invokedynamic` resolution. Method bodies are
//! decoded far enough to count instructions and name call targets.

// The transpile inventory is intentionally verbose in types: it is data the
// report prints verbatim.
#![allow(clippy::type_complexity)]
// Synthetic class files in the tests are built byte by byte, which the
// `manual_c_str_literals` lint misreads.
#![allow(clippy::manual_c_str_literals)]

pub mod bytecode;
pub mod report;
pub mod sample;

use std::collections::HashMap;

#[derive(Clone, Debug)]
pub enum Const {
    Utf8(String),
    Int(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Class(u16),
    Str(u16),
    Fieldref(u16, u16),
    Methodref(u16, u16),
    InterfaceMethodref(u16, u16),
    NameAndType(u16, u16),
    MethodHandle(u8, u16),
    MethodType(u16),
    Dynamic(u16, u16),
    InvokeDynamic(u16, u16),
    Module(u16),
    Package(u16),
    Empty,
}

#[derive(Clone, Debug)]
pub struct Method {
    pub name: String,
    pub descriptor: String,
    pub access: u16,
    pub max_stack: u16,
    pub max_locals: u16,
    pub code_len: usize,
    /// `(owner, name)` for every `invoke*` in the body.
    pub calls: Vec<(String, String)>,
    pub has_code: bool,
}

impl Method {
    pub fn is_static(&self) -> bool {
        self.access & 0x0008 != 0
    }
    pub fn is_native(&self) -> bool {
        self.access & 0x0100 != 0
    }
    pub fn is_abstract(&self) -> bool {
        self.access & 0x0400 != 0
    }
}

#[derive(Clone, Debug)]
pub struct ClassFile {
    pub major: u16,
    pub minor: u16,
    pub access: u16,
    pub name: String,
    pub super_name: String,
    pub interfaces: Vec<String>,
    pub methods: Vec<Method>,
    pub pool: Vec<Const>,
}

impl ClassFile {
    pub fn is_interface(&self) -> bool {
        self.access & 0x0200 != 0
    }
    pub fn is_public(&self) -> bool {
        self.access & 0x0001 != 0
    }

    /// `net/minecraft/world/level/Level` → `net.minecraft.world.level.Level`
    pub fn dotted(&self) -> String {
        self.name.replace('/', ".")
    }

    /// A stable Rust module path for the stub emitter.
    pub fn rust_module(&self) -> String {
        self.name.replace('/', "_")
    }
}

struct Cur<'a> {
    d: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    fn u8(&mut self) -> Result<u8, String> {
        let b = *self.d.get(self.i).ok_or("class: truncated")?;
        self.i += 1;
        Ok(b)
    }
    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.i + n > self.d.len() {
            return Err(format!("class: truncated (want {} at {})", n, self.i));
        }
        let s = &self.d[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }
    fn skip(&mut self, n: usize) -> Result<(), String> {
        self.take(n).map(|_| ())
    }
}

pub fn parse(data: &[u8]) -> Result<ClassFile, String> {
    if data.len() < 10 {
        return Err("class: too short".to_string());
    }
    if &data[0..4] != b"\xca\xfe\xba\xbe" {
        return Err("class: bad magic".to_string());
    }
    let mut c = Cur { d: data, i: 4 };
    let minor = c.u16()?;
    let major = c.u16()?;
    if !(45..=100).contains(&major) {
        return Err(format!("class: implausible major version {}", major));
    }
    let pool_count = c.u16()? as usize;
    let mut pool: Vec<Const> = Vec::with_capacity(pool_count);
    pool.push(Const::Empty);
    let mut i = 1usize;
    while i < pool_count {
        let tag = c.u8()?;
        let k = match tag {
            1 => {
                let n = c.u16()? as usize;
                let b = c.take(n)?;
                // MUTF-8 with NULs encoded as C0 80; this loosens it to UTF-8,
                // which is all the report needs.
                let s = String::from_utf8_lossy(b).replace('\u{0}', "");
                Const::Utf8(s)
            }
            3 => Const::Int(c.u32()? as i32),
            4 => Const::Float(f32::from_bits(c.u32()?)),
            5 => {
                let hi = c.u32()? as u64;
                let lo = c.u32()? as u64;
                Const::Long(((hi << 32) | lo) as i64)
            }
            6 => {
                let hi = c.u32()? as u64;
                let lo = c.u32()? as u64;
                Const::Double(f64::from_bits((hi << 32) | lo))
            }
            7 => Const::Class(c.u16()?),
            8 => Const::Str(c.u16()?),
            9 => Const::Fieldref(c.u16()?, c.u16()?),
            10 => Const::Methodref(c.u16()?, c.u16()?),
            11 => Const::InterfaceMethodref(c.u16()?, c.u16()?),
            12 => Const::NameAndType(c.u16()?, c.u16()?),
            15 => Const::MethodHandle(c.u8()?, c.u16()?),
            16 => Const::MethodType(c.u16()?),
            17 => Const::Dynamic(c.u16()?, c.u16()?),
            18 => Const::InvokeDynamic(c.u16()?, c.u16()?),
            19 => Const::Module(c.u16()?),
            20 => Const::Package(c.u16()?),
            other => return Err(format!("class: unknown constant tag {}", other)),
        };
        pool.push(k);
        // Long and Double occupy two slots.
        if tag == 5 || tag == 6 {
            pool.push(Const::Empty);
            i += 2;
        } else {
            i += 1;
        }
    }

    let access = c.u16()?;
    let this_idx = c.u16()?;
    let super_idx = c.u16()?;
    let name = class_name(&pool, this_idx)?;
    let super_name = if super_idx == 0 {
        String::new()
    } else {
        class_name(&pool, super_idx).unwrap_or_default()
    };
    let icount = c.u16()? as usize;
    let mut interfaces = Vec::with_capacity(icount);
    for _ in 0..icount {
        let idx = c.u16()?;
        interfaces.push(class_name(&pool, idx).unwrap_or_else(|_| "?".to_string()));
    }

    // fields: skip
    let fcount = c.u16()? as usize;
    for _ in 0..fcount {
        c.skip(6)?;
        let acount = c.u16()? as usize;
        for _ in 0..acount {
            c.skip(2)?;
            let len = c.u32()? as usize;
            c.skip(len)?;
        }
    }

    let mcount = c.u16()? as usize;
    let mut methods = Vec::with_capacity(mcount);
    for _ in 0..mcount {
        let m_access = c.u16()?;
        let name_idx = c.u16()?;
        let desc_idx = c.u16()?;
        let m_name = utf8(&pool, name_idx)?;
        let m_desc = utf8(&pool, desc_idx)?;
        let acount = c.u16()? as usize;
        let mut max_stack = 0;
        let mut max_locals = 0;
        let mut code_len = 0;
        let mut calls = Vec::new();
        let mut has_code = false;
        for _ in 0..acount {
            let aname_idx = c.u16()?;
            let alen = c.u32()? as usize;
            let aname = utf8(&pool, aname_idx).unwrap_or_default();
            if aname == "Code" {
                has_code = true;
                max_stack = c.u16()?;
                max_locals = c.u16()?;
                let clen = c.u32()? as usize;
                code_len = clen;
                let body = c.take(clen)?;
                calls = bytecode::invoke_targets(body, &pool);
                // exception table + nested attributes
                let etlen = c.u16()? as usize;
                c.skip(etlen * 8)?;
                let nattr = c.u16()? as usize;
                for _ in 0..nattr {
                    c.skip(2)?;
                    let len = c.u32()? as usize;
                    c.skip(len)?;
                }
            } else {
                c.skip(alen)?;
            }
        }
        methods.push(Method {
            name: m_name,
            descriptor: m_desc,
            access: m_access,
            max_stack,
            max_locals,
            code_len,
            calls,
            has_code,
        });
    }
    // class attributes: skip
    let acount = c.u16()? as usize;
    for _ in 0..acount {
        c.skip(2)?;
        let len = c.u32()? as usize;
        c.skip(len)?;
    }

    Ok(ClassFile {
        major,
        minor,
        access,
        name,
        super_name,
        interfaces,
        methods,
        pool,
    })
}

fn utf8(pool: &[Const], idx: u16) -> Result<String, String> {
    match pool.get(idx as usize) {
        Some(Const::Utf8(s)) => Ok(s.clone()),
        _ => Err(format!("class: constant {} is not UTF-8", idx)),
    }
}

fn class_name(pool: &[Const], idx: u16) -> Result<String, String> {
    match pool.get(idx as usize) {
        Some(Const::Class(n)) => utf8(pool, *n),
        _ => Err(format!("class: constant {} is not a Class", idx)),
    }
}

/// Aggregate across many class files.
#[derive(Default)]
pub struct ClassSummary {
    pub classes: usize,
    pub methods: usize,
    pub code_bytes: u64,
    pub call_edges: usize,
    /// `(target class, target method) -> call sites`
    pub hot: HashMap<(String, String), usize>,
    pub deepest: Vec<(String, usize)>,
    pub versions: HashMap<u16, usize>,
}

impl ClassSummary {
    pub fn add(&mut self, cf: &ClassFile) {
        self.classes += 1;
        *self.versions.entry(cf.major).or_insert(0) += 1;
        let mut bytes = 0usize;
        for m in &cf.methods {
            self.methods += 1;
            bytes += m.code_len;
            self.code_bytes += m.code_len as u64;
            for (owner, name) in &m.calls {
                self.call_edges += 1;
                *self.hot.entry((owner.clone(), name.clone())).or_insert(0) += 1;
            }
        }
        self.deepest.push((cf.name.clone(), bytes));
        if self.deepest.len() > 64 {
            self.deepest.sort_by_key(|entry| std::cmp::Reverse(entry.1));
            self.deepest.truncate(32);
        }
    }

    pub fn top_targets(&self, n: usize) -> Vec<((String, String), usize)> {
        let mut v: Vec<_> = self.hot.iter().map(|(k, c)| (k.clone(), *c)).collect();
        v.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        v.truncate(n);
        v
    }

    pub fn to_json(&self) -> String {
        use hz_core::Json;
        let mut versions: Vec<(String, Json)> = self
            .versions
            .iter()
            .map(|(k, v)| (format!("{}", k), Json::Num(*v as f64)))
            .collect();
        versions.sort_by(|a, b| a.0.cmp(&b.0));
        let targets = self
            .top_targets(32)
            .into_iter()
            .map(|((owner, name), count)| {
                Json::Obj(vec![
                    ("owner".into(), Json::Str(owner)),
                    ("method".into(), Json::Str(name)),
                    ("call_sites".into(), Json::Num(count as f64)),
                ])
            })
            .collect();
        Json::Obj(vec![
            ("classes".into(), Json::Num(self.classes as f64)),
            ("methods".into(), Json::Num(self.methods as f64)),
            ("bytecode_bytes".into(), Json::Num(self.code_bytes as f64)),
            ("call_edges".into(), Json::Num(self.call_edges as f64)),
            ("class_file_versions".into(), Json::Obj(versions)),
            ("hot_targets".into(), Json::Arr(targets)),
        ])
        .to_json_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tiny_class() {
        let bytes = sample::tiny_class("example/Subject", "java/lang/System", "gc");
        let cf = parse(&bytes).unwrap();
        assert_eq!(cf.name, "example/Subject");
        assert_eq!(cf.super_name, "java/lang/Object");
        assert_eq!(cf.major, 65);
        assert_eq!(cf.methods.len(), 1);
        let m = &cf.methods[0];
        assert_eq!(m.name, "run");
        assert!(m.is_static());
        assert!(m.has_code);
        assert_eq!(m.calls.len(), 1);
        assert_eq!(m.calls[0].0, "java/lang/System");
        assert_eq!(m.calls[0].1, "gc");
        assert_eq!(cf.rust_module(), "example_Subject");
    }

    #[test]
    fn rejects_non_class() {
        assert!(parse(b"not a class at all").is_err());
    }

    #[test]
    fn summary_counts() {
        let bytes = sample::tiny_class("a/B", "java/lang/System", "gc");
        let cf = parse(&bytes).unwrap();
        let mut s = ClassSummary::default();
        s.add(&cf);
        assert_eq!(s.classes, 1);
        assert_eq!(s.methods, 1);
        assert!(s.to_json().contains("hot_targets"));
    }
}
