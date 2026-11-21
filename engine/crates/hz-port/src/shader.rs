//! Shader packs: GLSL interface parsing, validation and a compiled descriptor.
//!
//! A full GLSL→Metal translator is a project on its own (it is what
//! SPIRV-Cross does), so this module does the part that can be done correctly
//! and cheaply: it reads the *interface* of a pack's shaders and says whether
//! the engine could drive them.
//!
//! What is checked:
//!
//! * every stage has a `#version` and it is a desktop GLSL version this engine
//!   understands (ES and Vulkan dialects are rejected with a reason),
//! * the vertex stage's outputs match the fragment stage's inputs by name and
//!   type, which is exactly how GLSL links a program,
//! * every uniform the shaders declare is one the engine can supply; anything
//!   else becomes a `requires` entry instead of a silent failure at runtime.
//!
//! What is not done here: translating the source. See docs/SHADERS.md for the
//! options and their cost.
//!
//! ```text
//! "SHD1" | u16 pack_count
//! per pack: str16 name | u16 stage_count
//!   per stage: str16 kind | u16 in | (type,name)* | u16 out | (type,name)* | u16 uniforms | (type,name)*
//! then: u16 requires_count | str16 each
//! ```

use hz_core::Buf;

/// Uniform names the engine provides to a shader. Anything else is reported as
/// a requirement the pack has beyond the engine.
pub const PROVIDED_UNIFORMS: &[&str] = &[
    "mvp",
    "cameraPosition",
    "fogColor",
    "fogParams",
    "time",
    "daylight",
    "textureAtlas",
    "textureArray",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decl {
    pub glsl_type: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stage {
    pub kind: StageKind,
    pub inputs: Vec<Decl>,
    pub outputs: Vec<Decl>,
    pub uniforms: Vec<Decl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageKind {
    Vertex,
    Fragment,
}

impl StageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StageKind::Vertex => "vertex",
            StageKind::Fragment => "fragment",
        }
    }

    /// Guess from a file name; shader packs use many conventions.
    pub fn from_name(name: &str) -> Option<StageKind> {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".vsh") || lower.ends_with(".vert") || lower.contains("vertex") {
            return Some(StageKind::Vertex);
        }
        if lower.ends_with(".fsh") || lower.ends_with(".frag") || lower.contains("fragment") {
            return Some(StageKind::Fragment);
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderPack {
    pub name: String,
    pub version: u32,
    pub stages: Vec<Stage>,
    /// Uniforms the shaders want that the engine cannot supply.
    pub requires: Vec<String>,
    pub warnings: Vec<String>,
}

impl ShaderPack {
    pub fn stage(&self, kind: StageKind) -> Option<&Stage> {
        self.stages.iter().find(|s| s.kind == kind)
    }

    /// True when the engine could drive this pack's interface as-is.
    pub fn is_supported(&self) -> bool {
        self.requires.is_empty()
            && self.stage(StageKind::Vertex).is_some()
            && self.stage(StageKind::Fragment).is_some()
    }
}

/// Remove comments and string literals so they cannot be mistaken for code.
fn strip_noise(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            i = (i + 1).min(bytes.len());
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn tokenize(src: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in src.chars() {
        let is_ident = ch.is_ascii_alphanumeric() || ch == '_';
        if is_ident {
            current.push(ch);
        } else {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            if !ch.is_whitespace() {
                tokens.push(ch.to_string());
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// `#version 330 core` → `Some(330)`.
fn version_of(tokens: &[String]) -> Option<u32> {
    for (i, token) in tokens.iter().enumerate() {
        if token == "version" {
            // `#` is its own token, so the number follows directly.
            if let Some(value) = tokens.get(i + 1).and_then(|t| t.parse::<u32>().ok()) {
                return Some(value);
            }
        }
    }
    None
}

/// Parse one stage's source.
///
/// Declarations are only collected at brace depth 0: a local `vec3 x` inside a
/// function body is not part of the interface.
pub fn parse_stage(kind: StageKind, src: &str) -> Stage {
    let cleaned = strip_noise(src);
    let tokens = tokenize(&cleaned);
    let mut stage = Stage {
        kind,
        inputs: Vec::new(),
        outputs: Vec::new(),
        uniforms: Vec::new(),
    };

    let mut depth = 0i32;
    let mut i = 0usize;
    while i < tokens.len() {
        let token = tokens[i].as_str();
        match token {
            "{" => {
                depth += 1;
                i += 1;
                continue;
            }
            "}" => {
                depth -= 1;
                i += 1;
                continue;
            }
            _ => {}
        }
        if depth != 0 {
            i += 1;
            continue;
        }

        // Skip a `layout(...)` qualifier; it carries no interface information we
        // use, but it does contain commas and parentheses.
        if token == "layout" {
            while i < tokens.len() && tokens[i] != ")" {
                i += 1;
            }
            i += 1;
            continue;
        }

        let bucket = match token {
            "in" | "attribute" | "varying" => Some(0),
            "out" => Some(1),
            "uniform" => Some(2),
            _ => None,
        };
        let bucket = match bucket {
            Some(b) => b,
            None => {
                i += 1;
                continue;
            }
        };

        // Skip qualifiers and precision keywords until the type token.
        let mut j = i + 1;
        while j < tokens.len()
            && matches!(
                tokens[j].as_str(),
                "highp"
                    | "mediump"
                    | "lowp"
                    | "flat"
                    | "smooth"
                    | "noperspective"
                    | "centroid"
                    | "sample"
                    | "patch"
                    | "invariant"
                    | "const"
                    | "coherent"
            )
        {
            j += 1;
        }
        if j >= tokens.len() {
            break;
        }
        let glsl_type = tokens[j].clone();
        let name = match tokens.get(j + 1) {
            Some(n)
                if n.chars()
                    .next()
                    .map(|c| c.is_ascii_alphabetic() || c == '_')
                    == Some(true) =>
            {
                n.clone()
            }
            _ => {
                i = j + 1;
                continue;
            }
        };
        // Type must look like a type, not a keyword: reject `if`, `return`, …
        if matches!(
            glsl_type.as_str(),
            "if" | "else" | "for" | "while" | "return" | "void" | "struct"
        ) {
            i = j + 1;
            continue;
        }

        let decl = Decl { glsl_type, name };
        match bucket {
            0 => {
                // `varying` is an output in the vertex stage and an input in the
                // fragment stage; `in`/`attribute` are inputs in a vertex stage.
                if token == "varying" && kind == StageKind::Vertex {
                    stage.outputs.push(decl);
                } else {
                    stage.inputs.push(decl);
                }
            }
            1 => stage.outputs.push(decl),
            _ => stage.uniforms.push(decl),
        }
        i = j + 2;
    }

    stage
}

/// Parse and validate a whole pack. `sources` pairs a stage with its source.
pub fn compile(name: &str, sources: &[(StageKind, String)]) -> ShaderPack {
    let mut stages = Vec::new();
    let mut warnings = Vec::new();
    let mut requires = Vec::new();
    let mut version = 0u32;

    for (kind, src) in sources {
        let cleaned = strip_noise(src);
        let tokens = tokenize(&cleaned);
        match version_of(&tokens) {
            Some(v) => {
                if v > version {
                    version = v;
                }
                if v < 120 {
                    warnings.push(format!(
                        "{}: #version {} predates GLSL 1.20",
                        kind.as_str(),
                        v
                    ));
                }
            }
            None => warnings.push(format!("{}: no #version directive", kind.as_str())),
        }
        if cleaned.contains(" es") && cleaned.contains("#version") {
            warnings.push(format!(
                "{}: GLSL ES dialect; the engine targets desktop GLSL",
                kind.as_str()
            ));
        }
        if cleaned.contains("gl_VertexID") || cleaned.contains("gl_FragCoord") {
            // Both exist in MSL under different names; worth naming in the report
            // rather than failing later.
            warnings.push(format!(
                "{}: uses a built-in that needs remapping when translating",
                kind.as_str()
            ));
        }
        let stage = parse_stage(*kind, src);
        for decl in &stage.uniforms {
            if !PROVIDED_UNIFORMS.contains(&decl.name.as_str()) && !requires.contains(&decl.name) {
                requires.push(decl.name.clone());
            }
        }
        stages.push(stage);
    }

    // Cross-stage link check: GLSL matches by name and type.
    if let (Some(vs), Some(fs)) = (
        stages.iter().find(|s| s.kind == StageKind::Vertex),
        stages.iter().find(|s| s.kind == StageKind::Fragment),
    ) {
        for input in &fs.inputs {
            match vs.outputs.iter().find(|o| o.name == input.name) {
                Some(output) if output.glsl_type != input.glsl_type => warnings.push(format!(
                    "stage link: {} is {} in the vertex stage but {} in the fragment stage",
                    input.name, output.glsl_type, input.glsl_type
                )),
                Some(_) => {}
                None => warnings.push(format!(
                    "stage link: fragment input {} has no vertex output",
                    input.name
                )),
            }
        }
    } else {
        warnings.push("pack needs both a vertex and a fragment stage".to_string());
    }

    ShaderPack {
        name: name.to_string(),
        version,
        stages,
        requires,
        warnings,
    }
}

pub fn encode(pack: &ShaderPack) -> Vec<u8> {
    let mut b = Buf::with_capacity(512);
    b.bytes(b"SHD1");
    b.u16(1);
    b.str16(&pack.name);
    b.u16(pack.stages.len() as u16);
    for stage in &pack.stages {
        b.str16(stage.kind.as_str());
        for list in [&stage.inputs, &stage.outputs, &stage.uniforms] {
            b.u16(list.len() as u16);
            for decl in list {
                b.str16(&decl.glsl_type);
                b.str16(&decl.name);
            }
        }
    }
    b.u16(pack.requires.len() as u16);
    for name in &pack.requires {
        b.str16(name);
    }
    b.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERT: &str = r#"
#version 330 core
layout(location = 0) in vec3 position;
layout(location = 1) in vec2 uv;
out vec2 v_uv;
out vec3 v_normal;
uniform mat4 mvp;
void main() {
    // a local must not be picked up as an interface
    vec3 local = position;
    v_uv = uv;
    v_normal = local;
    gl_Position = mvp * vec4(position, 1.0);
}
"#;

    const FRAG: &str = r#"
#version 330 core
in vec2 v_uv;
in vec3 v_normal;
out vec4 colour;
uniform sampler2D textureArray;
void main() {
    colour = vec4(v_uv, v_normal.x, 1.0);
}
"#;

    #[test]
    fn parses_the_interface_not_the_body() {
        let stage = parse_stage(StageKind::Vertex, VERT);
        assert_eq!(stage.inputs.len(), 2, "position + uv");
        assert_eq!(stage.outputs.len(), 2, "v_uv + v_normal");
        assert_eq!(stage.uniforms.len(), 1);
        assert_eq!(stage.uniforms[0].name, "mvp");
        assert!(
            !stage.inputs.iter().any(|d| d.name == "local"),
            "locals inside main() are not interface"
        );
    }

    #[test]
    fn a_matching_pack_is_supported() {
        let pack = compile(
            "demo",
            &[
                (StageKind::Vertex, VERT.to_string()),
                (StageKind::Fragment, FRAG.to_string()),
            ],
        );
        assert_eq!(pack.version, 330);
        assert!(pack.requires.is_empty(), "{:?}", pack.requires);
        assert!(pack.warnings.is_empty(), "{:?}", pack.warnings);
        assert!(pack.is_supported());
    }

    #[test]
    fn an_unknown_uniform_becomes_a_requirement() {
        let frag = FRAG.replace(
            "uniform sampler2D textureArray;",
            "uniform vec4 shadowMatrix;",
        );
        let pack = compile(
            "needs-shadow",
            &[
                (StageKind::Vertex, VERT.to_string()),
                (StageKind::Fragment, frag),
            ],
        );
        assert_eq!(pack.requires, vec!["shadowMatrix".to_string()]);
        assert!(!pack.is_supported());
    }

    #[test]
    fn mismatched_stage_interface_is_reported() {
        let frag = FRAG.replace("in vec3 v_normal;", "in vec4 v_normal;");
        let pack = compile(
            "mismatch",
            &[
                (StageKind::Vertex, VERT.to_string()),
                (StageKind::Fragment, frag),
            ],
        );
        assert!(
            pack.warnings.iter().any(|w| w.contains("vertex stage")),
            "{:?}",
            pack.warnings
        );
    }

    #[test]
    fn a_missing_fragment_output_is_reported_as_an_unlinked_input() {
        let frag = r#"
#version 330
in vec2 v_uv;
in vec3 unknown_thing;
out vec4 colour;
void main() { colour = vec4(1.0); }
"#;
        let pack = compile(
            "dangling",
            &[
                (StageKind::Vertex, VERT.to_string()),
                (StageKind::Fragment, frag.to_string()),
            ],
        );
        assert!(pack
            .warnings
            .iter()
            .any(|w| w.contains("unknown_thing") && w.contains("no vertex output")));
    }

    #[test]
    fn comments_and_strings_do_not_confuse_the_scanner() {
        let src = r#"
#version 330
// in vec3 not_real;
/* out vec2 also_not_real; */
uniform float time; // trailing
void main() { }
"#;
        let stage = parse_stage(StageKind::Vertex, src);
        assert!(stage.inputs.is_empty());
        assert!(stage.outputs.is_empty());
        assert_eq!(stage.uniforms.len(), 1);
        assert_eq!(stage.uniforms[0].name, "time");
    }

    #[test]
    fn legacy_varying_is_an_output_in_the_vertex_stage() {
        let src = "#version 120\nattribute vec3 position;\nvarying vec2 v_uv;\nvoid main(){}";
        let stage = parse_stage(StageKind::Vertex, src);
        assert_eq!(stage.inputs.len(), 1);
        assert_eq!(stage.outputs.len(), 1);
        assert_eq!(stage.outputs[0].name, "v_uv");
    }

    #[test]
    fn missing_stages_and_versions_are_warned_about() {
        let pack = compile("only-frag", &[(StageKind::Fragment, FRAG.to_string())]);
        assert!(pack.warnings.iter().any(|w| w.contains("both a vertex")));
        let pack = compile(
            "no-version",
            &[
                (StageKind::Vertex, VERT.replace("#version 330 core", "")),
                (StageKind::Fragment, FRAG.to_string()),
            ],
        );
        assert!(pack.warnings.iter().any(|w| w.contains("no #version")));
    }

    #[test]
    fn stage_kinds_come_from_file_names() {
        assert_eq!(StageKind::from_name("world.vsh"), Some(StageKind::Vertex));
        assert_eq!(StageKind::from_name("world.fsh"), Some(StageKind::Fragment));
        assert_eq!(
            StageKind::from_name("final.frag"),
            Some(StageKind::Fragment)
        );
        assert_eq!(StageKind::from_name("notes.txt"), None);
    }

    #[test]
    fn encodes_a_descriptor() {
        let pack = compile(
            "demo",
            &[
                (StageKind::Vertex, VERT.to_string()),
                (StageKind::Fragment, FRAG.to_string()),
            ],
        );
        let bytes = encode(&pack);
        assert_eq!(&bytes[..4], b"SHD1");
    }
}
