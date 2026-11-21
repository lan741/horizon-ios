//! Runtime view of the shader packs in a `.hzp`.
//!
//! The porter compiles a `SHD1` descriptor per pack; this module decodes it into
//! something the app can show. The engine does not *execute* these shaders: a
//! GLSL→MSL translation step is still missing, and pretending otherwise would
//! be the one thing this project has consistently refused to do. What the
//! runtime can state exactly is: which packs exist, what interface they declare,
//! and which of them ask for uniforms the engine does not provide.
//!
//! See docs/SHADERS.md for the translation options.

use hz_core::util::Reader;
use hz_core::Pack;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderDecl {
    pub glsl_type: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderStage {
    pub kind: String,
    pub inputs: Vec<ShaderDecl>,
    pub outputs: Vec<ShaderDecl>,
    pub uniforms: Vec<ShaderDecl>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderPackInfo {
    pub name: String,
    /// Uniforms the pack needs that the engine cannot supply.
    pub requires: Vec<String>,
    pub stages: Vec<ShaderStage>,
    /// True when the interface is complete and nothing is missing.
    pub supported: bool,
    /// Translated Metal source, when the porter managed to produce it.
    pub msl: Option<Vec<u8>>,
}

impl ShaderPackInfo {
    pub fn stage(&self, kind: &str) -> Option<&ShaderStage> {
        self.stages.iter().find(|s| s.kind == kind)
    }

    /// One line for the UI: "world — vertex+fragment, needs shadowMatrix".
    pub fn summary(&self) -> String {
        let kinds: Vec<&str> = self.stages.iter().map(|s| s.kind.as_str()).collect();
        if self.requires.is_empty() {
            format!("{} — {}", self.name, kinds.join("+"))
        } else {
            format!(
                "{} — {}, needs {}",
                self.name,
                kinds.join("+"),
                self.requires.join(", ")
            )
        }
    }
}

fn decode(data: &[u8]) -> Result<Vec<ShaderPackInfo>, String> {
    let mut r = Reader::new(data);
    if r.take(4)? != b"SHD1" {
        return Err("shader: bad magic".to_string());
    }
    let count = r.u16()? as usize;
    let mut packs = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let name = r.str16()?;
        let stage_count = r.u16()? as usize;
        let mut stages = Vec::with_capacity(stage_count.min(8));
        for _ in 0..stage_count {
            let kind = r.str16()?;
            let mut lists: [Vec<ShaderDecl>; 3] = [Vec::new(), Vec::new(), Vec::new()];
            for list in lists.iter_mut() {
                let n = r.u16()? as usize;
                for _ in 0..n {
                    let glsl_type = r.str16()?;
                    let decl_name = r.str16()?;
                    list.push(ShaderDecl {
                        glsl_type,
                        name: decl_name,
                    });
                }
            }
            let [inputs, outputs, uniforms] = lists;
            stages.push(ShaderStage {
                kind,
                inputs,
                outputs,
                uniforms,
            });
        }
        let requires_count = r.u16()? as usize;
        let mut requires = Vec::with_capacity(requires_count.min(64));
        for _ in 0..requires_count {
            requires.push(r.str16()?);
        }
        let has_both = stages.iter().any(|s| s.kind == "vertex")
            && stages.iter().any(|s| s.kind == "fragment");
        packs.push(ShaderPackInfo {
            name,
            supported: requires.is_empty() && has_both,
            requires,
            stages,
            msl: None,
        });
    }
    Ok(packs)
}

/// Every shader pack in the container, in pack order.
pub fn collect(pack: &Pack) -> Vec<ShaderPackInfo> {
    let mut out = Vec::new();
    for entry in &pack.entries {
        if entry.kind != hz_core::kind::SHADER {
            continue;
        }
        match decode(&entry.data) {
            Ok(mut packs) => out.append(&mut packs),
            // A malformed descriptor must not stop the world from loading.
            Err(_) => continue,
        }
    }
    // Attach the translated source, keyed by the same pack name.
    for source in &pack.entries {
        if source.kind != hz_core::kind::SHADER_MSL {
            continue;
        }
        let name = source
            .name
            .strip_prefix("shaders-msl/")
            .unwrap_or(&source.name)
            .rsplit('/')
            .next()
            .unwrap_or("");
        if let Some(info) = out.iter_mut().find(|p| p.name == name) {
            info.msl = Some(source.data.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hz_core::kind;

    fn descriptor(name: &str, requires: &[&str]) -> Vec<u8> {
        let mut b = hz_core::Buf::new();
        b.bytes(b"SHD1");
        b.u16(1);
        b.str16(name);
        b.u16(2);
        for (kind_name, ins, outs, uni) in [("vertex", 1u16, 1u16, 1u16), ("fragment", 1, 1, 1)] {
            b.str16(kind_name);
            for count in [ins, outs, uni] {
                b.u16(count);
                for _ in 0..count {
                    b.str16("vec4");
                    b.str16("x");
                }
            }
        }
        b.u16(requires.len() as u16);
        for r in requires {
            b.str16(r);
        }
        b.finish()
    }

    #[test]
    fn decodes_packs_and_flags_requirements() {
        let mut pack = Pack::new();
        pack.add(kind::SHADER, "shaders/world", descriptor("world", &[]));
        pack.add(
            kind::SHADER,
            "shaders/shadow",
            descriptor("shadow", &["shadowMatrix"]),
        );
        let packs = collect(&pack);
        assert_eq!(packs.len(), 2);
        assert!(packs[0].supported);
        assert!(!packs[1].supported);
        assert_eq!(packs[1].requires, vec!["shadowMatrix".to_string()]);
        assert!(packs[1].summary().contains("shadowMatrix"));
        assert_eq!(packs[0].stage("vertex").unwrap().inputs.len(), 1);
    }

    #[test]
    fn translated_source_attaches_to_its_pack() {
        let mut pack = Pack::new();
        pack.add(kind::SHADER, "shaders/world", descriptor("world", &[]));
        pack.add(
            kind::SHADER_MSL,
            "shaders-msl/world",
            b"// generated\nvertex void main() {}".to_vec(),
        );
        let packs = collect(&pack);
        assert_eq!(packs.len(), 1);
        let msl = packs[0].msl.as_ref().expect("msl attached");
        assert!(std::str::from_utf8(msl).unwrap().contains("generated"));
    }

    #[test]
    fn a_broken_descriptor_is_skipped_not_fatal() {
        let mut pack = Pack::new();
        pack.add(kind::SHADER, "shaders/bad", b"NOPE".to_vec());
        pack.add(kind::SHADER, "shaders/ok", descriptor("ok", &[]));
        let packs = collect(&pack);
        assert_eq!(packs.len(), 1, "the good one still loads");
        assert_eq!(packs[0].name, "ok");
    }
}
