//! hz-port — "recompile Minecraft: Java Edition into Rust" as far as it can
//! honestly go on an iPad.
//!
//! The Java *code* is not executed and not translated on device (iOS has no
//! JIT and no process exec; see docs/IOS_CONSTRAINTS.md). What happens here is
//! the same trade Horizon makes: everything that is *data* in the jar —
//! blockstates, models+geometry, textures, language files, recipes, loot
//! tables, worldgen, mixin descriptors — is compiled into native, zero-parse
//! `.hzp` descriptors that the Rust engine consumes directly. The remaining
//! Java classes are reported as `unported` for the Mac/CI transpile tier.
//!
//! Pipeline stages (each one reports progress):
//!   read → blockstates → models → textures → lang → data → mixins → classes → write

// Pipeline state is a flat list of tuples by design; naming each one would add
// noise without adding meaning.
#![allow(clippy::type_complexity)]

pub mod animations;
pub mod blockstates;
pub mod glsl;
pub mod model;
pub mod report;
pub mod shader;
pub mod sounds;

use hz_core::{kind, Json, Pack};

pub use report::{Report, Stage};

/// Options for one port run. Defaults are what the iPad app uses.
#[derive(Clone, Debug)]
pub struct Options {
    /// Minecraft version string taken from the version JSON ("26.3").
    pub minecraft_version: String,
    /// Engine ABI the pack was compiled for; the app refuses other majors.
    pub engine_api: u32,
    /// Languages to keep. Empty = every language in the jar.
    pub languages: Vec<String>,
    /// Keep `.class` names in the report (they are never emitted as data).
    pub list_classes: bool,
    /// Parse every `.class` body into the JVM inventory (call graph, bytecode
    /// volume). Costs a little import time; the iPad app keeps it on because
    /// the "what is still Java" screen needs the numbers.
    pub analyze_classes: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            minecraft_version: "unknown".to_string(),
            engine_api: 1,
            languages: Vec::new(),
            list_classes: true,
            analyze_classes: true,
        }
    }
}

pub struct PortResult {
    pub pack: Pack,
    pub report: Report,
}

impl PortResult {
    pub fn report_json(&self) -> String {
        self.report.to_json()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryClass {
    BlockState,
    Model,
    Texture,
    Lang,
    Sound,
    Data,
    Mixin,
    McMeta,
    /// `*.png.mcmeta`: the animation descriptor for the texture beside it.
    AnimMeta,
    /// A GLSL stage file from a shader pack.
    Shader,
    JavaClass,
    Other,
}

/// Classify one jar entry purely by path.
pub fn classify(name: &str) -> EntryClass {
    if name.ends_with('/') {
        return EntryClass::Other;
    }
    if name.ends_with(".class") {
        return EntryClass::JavaClass;
    }
    if name.ends_with(".mixins.json") || name.contains(".mixins.") {
        return EntryClass::Mixin;
    }
    if name.ends_with(".png.mcmeta") {
        return EntryClass::AnimMeta;
    }
    if name == "pack.mcmeta" {
        return EntryClass::McMeta;
    }
    if let Some(rest) = name.strip_prefix("assets/") {
        let mut it = rest.splitn(2, '/');
        let _ns = it.next().unwrap_or("");
        let tail = it.next().unwrap_or("");
        if tail.starts_with("blockstates/") && tail.ends_with(".json") {
            return EntryClass::BlockState;
        }
        if tail.starts_with("models/") && tail.ends_with(".json") {
            return EntryClass::Model;
        }
        if tail.starts_with("textures/") {
            return EntryClass::Texture;
        }
        if tail.starts_with("lang/") && tail.ends_with(".json") {
            return EntryClass::Lang;
        }
        if tail == "sounds.json" {
            return EntryClass::Sound;
        }
        if tail.starts_with("sounds/") && tail.ends_with(".ogg") {
            return EntryClass::Sound;
        }
        return EntryClass::Other;
    }
    if name.starts_with("data/") {
        return EntryClass::Data;
    }
    if shader_stage_of(name).is_some() {
        return EntryClass::Shader;
    }
    EntryClass::Other
}

/// `assets/<ns>/models/block/stone.json` → `minecraft:block/stone`
pub fn model_id(name: &str) -> Option<String> {
    let rest = name.strip_prefix("assets/")?;
    let (ns, tail) = rest.split_once('/')?;
    let tail = tail.strip_prefix("models/")?;
    let tail = tail.strip_suffix(".json")?;
    Some(format!("{}:{}", ns, tail))
}

/// `assets/<ns>/blockstates/stone.json` → `minecraft:stone`
pub fn block_id(name: &str) -> Option<String> {
    let rest = name.strip_prefix("assets/")?;
    let (ns, tail) = rest.split_once('/')?;
    let tail = tail.strip_prefix("blockstates/")?;
    let tail = tail.strip_suffix(".json")?;
    Some(format!("{}:{}", ns, tail))
}

/// `assets/<ns>/textures/block/stone.png` → `minecraft:block/stone`
pub fn texture_id(name: &str) -> Option<String> {
    let rest = name.strip_prefix("assets/")?;
    let (ns, tail) = rest.split_once('/')?;
    let tail = tail.strip_prefix("textures/")?;
    let tail = tail.strip_suffix(".png")?;
    Some(format!("{}:{}", ns, tail))
}

/// Run the whole pipeline over the bytes of a `client.jar` (or a resource-pack
/// zip; anything ZIP with assets/data works).
pub fn port<F>(jar: &[u8], opts: &Options, mut progress: F) -> Result<PortResult, String>
where
    F: FnMut(Stage, u32, u32),
{
    let zip = hz_core::zipfile::Zip::open(jar)?;
    let entries = zip.entries().to_vec();
    let mut report = Report::new(opts, jar, &zip);

    let mut pack = Pack::new();
    let total = entries.len() as u32;

    // ---- pass 1: read + classify -------------------------------------------
    progress(Stage::Read, 0, total);
    let mut blockstate_src: Vec<(String, Vec<u8>)> = Vec::new();
    let mut model_src: Vec<(String, Vec<u8>)> = Vec::new();
    let mut pending_textures: Vec<(String, usize)> = Vec::new();
    let mut pending_langs: Vec<(String, usize)> = Vec::new();
    let mut pending_data: Vec<(String, usize)> = Vec::new();
    let mut pending_mixins: Vec<(String, usize)> = Vec::new();
    let mut pending_anim_meta: Vec<(String, usize)> = Vec::new();
    let mut pending_sound_json: Vec<(String, usize)> = Vec::new();
    let mut pending_ogg: Vec<(String, usize)> = Vec::new();
    let mut pending_shaders: Vec<(String, Option<shader::StageKind>, usize)> = Vec::new();

    for (i, e) in entries.iter().enumerate() {
        if e.is_dir {
            continue;
        }
        let class = classify(&e.name);
        report.count(class);
        match class {
            EntryClass::BlockState => match zip.read_index(i) {
                Ok(b) => {
                    if let Some(id) = block_id(&e.name) {
                        blockstate_src.push((id, b));
                    }
                }
                Err(err) => report.warn(err),
            },
            EntryClass::Model => match zip.read_index(i) {
                Ok(b) => {
                    if let Some(id) = model_id(&e.name) {
                        model_src.push((id, b));
                    }
                }
                Err(err) => report.warn(err),
            },
            EntryClass::Texture => pending_textures.push((e.name.clone(), i)),
            EntryClass::Lang => pending_langs.push((e.name.clone(), i)),
            EntryClass::Data => pending_data.push((e.name.clone(), i)),
            EntryClass::Mixin => pending_mixins.push((e.name.clone(), i)),
            EntryClass::AnimMeta => pending_anim_meta.push((e.name.clone(), i)),
            EntryClass::Shader => {
                pending_shaders.push((e.name.clone(), shader_stage_of(&e.name), i))
            }
            EntryClass::Sound => {
                if e.name.ends_with(".ogg") {
                    pending_ogg.push((e.name.clone(), i));
                } else {
                    pending_sound_json.push((e.name.clone(), i));
                }
            }
            EntryClass::JavaClass => {
                report.note_class(&e.name);
                if opts.analyze_classes {
                    match zip.read_index(i) {
                        Ok(bytes) => report.add_class(&bytes, &e.name),
                        Err(err) => report.warn(err),
                    }
                }
            }
            _ => {}
        }
        if i % 512 == 0 {
            progress(Stage::Read, i as u32, total);
        }
    }

    // ---- pass 2: blockstates ----------------------------------------------
    let bs_total = blockstate_src.len() as u32;
    for (i, (id, bytes)) in blockstate_src.iter().enumerate() {
        let text = crate::de_bom(bytes);
        match hz_core::Json::parse(&text) {
            Ok(j) => match blockstates::compile(&j) {
                Ok(bin) => pack.add(kind::BLOCKSTATES, format!("blockstates/{}", id), bin),
                Err(err) => {
                    report.fail(format!("blockstate {}: {}", id, err));
                }
            },
            Err(err) => report.fail(format!("blockstate {}: {}", id, err)),
        }
        progress(Stage::BlockStates, i as u32, bs_total);
    }

    // ---- pass 3: models (two-pass, parent chains resolved) ----------------
    let md_total = model_src.len() as u32;
    for (i, (id, bytes)) in model_src.iter().enumerate() {
        let text = crate::de_bom(bytes);
        match hz_core::Json::parse(&text) {
            Ok(j) => match model::compile(id, &j, &model_src) {
                Ok(bin) => pack.add(kind::MODEL, format!("models/{}", id), bin),
                Err(err) => report.fail(format!("model {}: {}", id, err)),
            },
            Err(err) => report.fail(format!("model {}: {}", id, err)),
        }
        progress(Stage::Models, i as u32, md_total);
    }

    // ---- pass 4: textures (raw PNG, verified magic) -----------------------
    let tx_total = pending_textures.len() as u32;
    for (i, (name, idx)) in pending_textures.iter().enumerate() {
        match zip.read_index(*idx) {
            Ok(bytes) => {
                if bytes.len() >= 8 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    if let Some(id) = texture_id(name) {
                        pack.add(kind::TEXTURE, format!("textures/{}", id), bytes);
                    }
                } else {
                    report.fail(format!("texture {} is not a PNG", name));
                }
            }
            Err(err) => report.warn(err),
        }
        progress(Stage::Textures, i as u32, tx_total);
    }

    // ---- pass 5: language tables ------------------------------------------
    let lang_total = pending_langs.len() as u32;
    for (i, (name, idx)) in pending_langs.iter().enumerate() {
        let code = name
            .rsplit('/')
            .next()
            .unwrap_or("unknown.json")
            .trim_end_matches(".json");
        if !opts.languages.is_empty() && !opts.languages.iter().any(|l| l == code) {
            progress(Stage::Lang, i as u32, lang_total);
            continue;
        }
        match zip.read_index(*idx) {
            Ok(bytes) => match hz_core::Json::parse(&crate::de_bom(&bytes)) {
                Ok(Json::Obj(pairs)) => {
                    let mut buf = hz_core::Buf::with_capacity(bytes.len());
                    let mut table: Vec<(String, String)> = pairs
                        .into_iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                        .collect();
                    table.sort_by(|a, b| a.0.cmp(&b.0));
                    buf.u32(table.len() as u32);
                    for (k, v) in &table {
                        buf.str16(k);
                        buf.str16(v);
                    }
                    pack.add(kind::LANG, format!("lang/{}", code), buf.finish());
                }
                _ => report.fail(format!("lang {} is not a JSON object", name)),
            },
            Err(err) => report.warn(err),
        }
        progress(Stage::Lang, i as u32, lang_total);
    }

    // ---- pass 6: data (canonical JSON bytes) ------------------------------
    let dt_total = pending_data.len() as u32;
    for (i, (name, idx)) in pending_data.iter().enumerate() {
        match zip.read_index(*idx) {
            Ok(bytes) => {
                let rel = name.strip_prefix("data/").unwrap_or(name.as_str());
                match hz_core::Json::parse(&crate::de_bom(&bytes)) {
                    Ok(j) => pack.add(
                        kind::DATA,
                        format!("data/{}", rel),
                        j.to_json_string().into_bytes(),
                    ),
                    Err(_) => pack.add(kind::DATA, format!("data/{}", rel), bytes),
                }
            }
            Err(err) => report.warn(err),
        }
        progress(Stage::Data, i as u32, dt_total);
    }

    // ---- pass 7: mixin descriptors ----------------------------------------
    let mx_total = pending_mixins.len() as u32;
    for (i, (name, idx)) in pending_mixins.iter().enumerate() {
        match zip.read_index(*idx) {
            Ok(bytes) => {
                let rel = name.trim_end_matches(".json");
                pack.add(kind::MIXIN, format!("mixins/{}", rel), bytes);
            }
            Err(err) => report.warn(err),
        }
        progress(Stage::Mixins, i as u32, mx_total);
    }

    // ---- pass 7b: texture animations --------------------------------------
    // A `.png.mcmeta` beside a texture describes a vertical strip of frames.
    // We do not split the PNG: the descriptor carries the frame count and
    // timing, and the mesher offsets V when it bakes a quad.
    let an_total = pending_anim_meta.len() as u32;
    for (i, (name, idx)) in pending_anim_meta.iter().enumerate() {
        let texture_path = name.trim_end_matches(".mcmeta"); // .../stone.png
        let texture_id = texture_id(texture_path);
        match (texture_id, zip.read_index(*idx)) {
            (Some(id), Ok(bytes)) => match animations::compile(&id, &crate::de_bom(&bytes)) {
                Ok(Some(bin)) => pack.add(kind::ANIM, format!("anim/{}", id), bin),
                Ok(None) => {}
                Err(err) => report.fail(format!("animation {}: {}", name, err)),
            },
            (None, _) => report.fail(format!("animation {} has no sibling png", name)),
            (_, Err(err)) => report.warn(err),
        }
        progress(Stage::Textures, i as u32, an_total);
    }

    // ---- pass 7c: sounds ---------------------------------------------------
    // Ogg payloads are kept byte for byte: decoding is the platform's job. The
    // catalog is compiled so the runtime can resolve an event name to a pack
    // entry without touching JSON.
    let snd_total = (pending_sound_json.len() + pending_ogg.len()) as u32;
    let mut sound_events = 0usize;
    for (i, (name, idx)) in pending_sound_json.iter().enumerate() {
        let namespace = name
            .strip_prefix("assets/")
            .and_then(|rest| rest.split('/').next())
            .unwrap_or("minecraft");
        match zip.read_index(*idx) {
            Ok(bytes) => match sounds::compile(namespace, &crate::de_bom(&bytes)) {
                Ok(events) => {
                    sound_events += events.len();
                    pack.add(
                        kind::SOUND,
                        format!("soundcatalog/{}", namespace),
                        sounds::encode(&events),
                    );
                }
                Err(err) => report.fail(format!("sounds.json {}: {}", name, err)),
            },
            Err(err) => report.warn(err),
        }
        progress(Stage::Data, i as u32, snd_total);
    }
    for (i, (name, idx)) in pending_ogg.iter().enumerate() {
        match zip.read_index(*idx) {
            Ok(bytes) => {
                // Validate the container as it goes in, so a broken file is
                // reported at import time rather than at playback time.
                match hz_core::ogg::parse(&bytes) {
                    Ok(info) => {
                        if !info.checksums_ok {
                            report.warn(format!("{}: ogg page checksum mismatch", name));
                        }
                        let id = name
                            .strip_prefix("assets/")
                            .and_then(|rest| rest.split_once('/'))
                            .map(|(ns, tail)| {
                                let tail = tail.strip_prefix("sounds/").unwrap_or(tail);
                                format!("{}:{}", ns, tail.trim_end_matches(".ogg"))
                            })
                            .unwrap_or_else(|| name.clone());
                        pack.add(kind::SOUND, format!("sounds/{}", id), bytes);
                    }
                    Err(err) => report.fail(format!("{}: {}", name, err)),
                }
            }
            Err(err) => report.warn(err),
        }
        progress(
            Stage::Data,
            (pending_sound_json.len() + i) as u32,
            snd_total,
        );
    }

    // ---- pass 7d: shader packs --------------------------------------------
    // Every directory that has both a vertex and a fragment stage becomes one
    // pack. The descriptor records the interface and what the pack needs beyond
    // the engine; translating the source is a separate problem (docs/SHADERS.md).
    let sh_total = pending_shaders.len() as u32;
    let mut shader_packs = 0usize;
    {
        use std::collections::BTreeMap;
        let mut by_dir: BTreeMap<String, Vec<(shader::StageKind, String)>> = BTreeMap::new();
        for (i, (name, kind, index)) in pending_shaders.iter().enumerate() {
            let kind = match kind {
                Some(k) => *k,
                None => {
                    progress(Stage::Data, i as u32, sh_total);
                    continue;
                }
            };
            match zip.read_index(*index) {
                Ok(bytes) => {
                    let dir = name
                        .rsplit_once('/')
                        .map(|(d, _)| d.to_string())
                        .unwrap_or_default();
                    by_dir
                        .entry(dir)
                        .or_default()
                        .push((kind, crate::de_bom(&bytes)));
                }
                Err(err) => report.warn(err),
            }
            progress(Stage::Data, i as u32, sh_total);
        }
        for (dir, sources) in by_dir {
            let pack_name = dir
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("shader-pack")
                .to_string();
            let compiled = shader::compile(&pack_name, &sources);
            for warning in &compiled.warnings {
                report.warn(format!("shader {}: {}", pack_name, warning));
            }
            if !compiled.requires.is_empty() {
                report.warn(format!(
                    "shader {}: needs uniforms the engine does not provide: {}",
                    pack_name,
                    compiled.requires.join(", ")
                ));
            }
            shader_packs += 1;
            let rel = dir.trim_start_matches("shaders/").to_string();
            // Translate here, at import: the pack then carries ready-to-compile
            // Metal source and the runtime never needs a GLSL front end.
            match glsl::translate(&compiled, &sources) {
                Ok(translated) => {
                    for note in &translated.notes {
                        report.warn(format!("shader {}: {}", pack_name, note));
                    }
                    pack.add(
                        kind::SHADER_MSL,
                        format!("shaders-msl/{}", rel),
                        translated.msl.into_bytes(),
                    );
                }
                Err(diagnostics) => {
                    // Refused packs still ship their descriptor so the UI can
                    // explain why, but no MSL is produced.
                    for d in diagnostics {
                        report.warn(format!("shader {}: refused: {}", pack_name, d));
                    }
                }
            }
            pack.add(
                kind::SHADER,
                format!("shaders/{}", rel),
                shader::encode(&compiled),
            );
        }
    }

    // ---- pass 8: manifest --------------------------------------------------
    progress(Stage::Write, 0, 1);
    report.sound_events = sound_events;
    report.shader_packs = shader_packs;
    report.finish(&pack);
    let manifest = report.manifest_json(&pack);
    pack.add(kind::MANIFEST, "manifest", manifest.into_bytes());
    report.pack_entries = pack.entries.len();
    progress(Stage::Write, 1, 1);

    Ok(PortResult { pack, report })
}

/// Emit one Rust stub file per `.class`, for the Mac/CI transpile tier.
/// Returns `(files_written, modules_skipped)`.
pub fn emit_stubs(jar: &[u8], out_dir: &str) -> Result<(usize, usize), String> {
    let zip = hz_core::zipfile::Zip::open(jar)?;
    std::fs::create_dir_all(out_dir).map_err(|e| format!("cannot create {}: {}", out_dir, e))?;
    let mut written = 0usize;
    let mut skipped = 0usize;
    for (i, e) in zip.entries().iter().enumerate() {
        if e.is_dir || !e.name.ends_with(".class") {
            continue;
        }
        match zip.read_index(i) {
            Ok(bytes) => match hz_class::parse(&bytes) {
                Ok(cf) => {
                    let src = hz_class::report::emit_module(&cf);
                    let file = format!("{}/{}.rs", out_dir, cf.rust_module());
                    match std::fs::write(&file, src) {
                        Ok(()) => written += 1,
                        Err(_) => skipped += 1,
                    }
                }
                Err(_) => skipped += 1,
            },
            Err(_) => skipped += 1,
        }
    }
    Ok((written, skipped))
}

/// Decode a jar entry as text, dropping a UTF-8 BOM if present.
pub fn de_bom(bytes: &[u8]) -> String {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    String::from_utf8_lossy(b).into_owned()
}

/// Shader stage for a path, when it looks like one.
pub fn shader_stage_of(name: &str) -> Option<shader::StageKind> {
    if !(name.starts_with("shaders/") || name.contains("/shaders/")) {
        return None;
    }
    shader::StageKind::from_name(name)
}

/// Parse `"facing=north,half=top"` into sorted (name, value) pairs.
pub fn parse_props(s: &str) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('=') {
            Some((k, val)) => v.push((k.trim().to_string(), val.trim().to_string())),
            None => v.push((part.to_string(), String::new())),
        }
    }
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a stored-only ZIP in memory so the tests do not depend on an
    /// external archiver. Real jars exercise the DEFLATE path; see
    /// scripts/port-demo.ps1 for the end-to-end check.
    pub fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let off = out.len() as u32;
            let crc = {
                let mut table = [0u32; 256];
                #[allow(clippy::needless_range_loop)]
                for i in 0..256 {
                    let mut c = i as u32;
                    for _ in 0..8 {
                        c = if c & 1 != 0 {
                            0xEDB88320 ^ (c >> 1)
                        } else {
                            c >> 1
                        };
                    }
                    table[i] = c;
                }
                let mut crc = 0xFFFF_FFFFu32;
                for &b in data.iter() {
                    crc = table[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
                }
                crc ^ 0xFFFF_FFFF
            };
            out.extend_from_slice(b"PK\x03\x04");
            // version(2) flags(2) method(2) time(2) date(2)
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);

            central.extend_from_slice(b"PK\x01\x02");
            // made-by(2) needed(2) flags(2) method(2) time(2) date(2)
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&off.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn sample_jar() -> Vec<u8> {
        let blockstates = br#"{"variants":{"facing=north":{"model":"minecraft:block/stone","x":0},"facing=south":{"model":"minecraft:block/stone","y":180}}}"#;
        let model = br##"{"parent":"minecraft:block/cube_all","textures":{"particle":"minecraft:block/stone"}}"##;
        let model_cube = br##"{"textures":{"all":"minecraft:block/stone"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all"},"down":{"texture":"#all"},"north":{"texture":"#all"}}}]}"##;
        let lang = br#"{"block.minecraft.stone":"Stone","item.minecraft.apple":"Apple"}"#;
        let recipe = br#"{ "type": "minecraft:crafting_shaped", "pattern": ["X"], "key": {"X": {"item": "minecraft:stone"}} }"#;
        let png: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRfake-but-magic-ok";
        let mixin = br#"{"required":true,"package":"com.example.mixin","mixins":["ExampleMixin"]}"#;
        build_zip(&[
            ("assets/minecraft/blockstates/stone.json", blockstates),
            ("assets/minecraft/models/block/stone.json", model),
            ("assets/minecraft/models/block/cube_all.json", model_cube),
            ("assets/minecraft/textures/block/stone.png", png),
            (
                "assets/minecraft/textures/block/stone.png.mcmeta",
                br#"{"animation":{"frametime":2,"frames":[0,1,2,3]}}"#,
            ),
            ("assets/minecraft/lang/en_us.json", lang),
            ("data/minecraft/recipe/stone.json", recipe),
            ("example.mixins.json", mixin),
            ("net/minecraft/world/level/Level.class", b"\xca\xfe\xba\xbe"),
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n"),
        ])
    }

    #[test]
    fn classifies_paths() {
        assert_eq!(
            classify("assets/x/blockstates/a.json"),
            EntryClass::BlockState
        );
        assert_eq!(classify("assets/x/models/item/a.json"), EntryClass::Model);
        assert_eq!(classify("assets/x/textures/a.png"), EntryClass::Texture);
        assert_eq!(classify("assets/x/lang/en_us.json"), EntryClass::Lang);
        assert_eq!(classify("data/x/recipe/a.json"), EntryClass::Data);
        assert_eq!(classify("a/b/C.class"), EntryClass::JavaClass);
    }

    #[test]
    fn ports_sample_jar() {
        let jar = sample_jar();
        let mut seen = Vec::new();
        let res = port(
            &jar,
            &Options {
                minecraft_version: "26.3".into(),
                ..Default::default()
            },
            |stage, _, _| seen.push(stage),
        )
        .unwrap();
        assert!(seen.contains(&Stage::BlockStates));
        assert!(seen.contains(&Stage::Write));
        let p = res.pack;
        assert_eq!(p.count_kind(kind::BLOCKSTATES), 1);
        assert_eq!(p.count_kind(kind::MODEL), 2);
        assert_eq!(p.count_kind(kind::TEXTURE), 1);
        assert_eq!(p.count_kind(kind::LANG), 1);
        assert_eq!(p.count_kind(kind::DATA), 1);
        assert_eq!(p.count_kind(kind::MIXIN), 1);
        assert_eq!(p.count_kind(kind::ANIM), 1);
        let anim = hz_core::anim::Animation::from_bytes(
            &p.get("anim/minecraft:block/stone").unwrap().data,
        )
        .unwrap();
        assert_eq!(anim.frametime, 2);
        assert_eq!(anim.frame_count(), 4);
        assert_eq!(p.count_kind(kind::MANIFEST), 1);
        assert!(res.report.java_classes >= 1);

        // the pack must survive a round-trip through its on-disk form
        let bytes = p.write();
        let back = hz_core::Pack::read(&bytes).unwrap();
        assert_eq!(back.entries.len(), p.entries.len());
        assert!(back.get("lang/en_us").is_some());
        assert!(back.get("manifest").is_some());
    }

    #[test]
    fn compiles_a_shader_pack_from_directory() {
        let vert = r#"#version 330 core
layout(location = 0) in vec3 position;
out vec2 v_uv;
uniform mat4 mvp;
void main() { v_uv = position.xy; gl_Position = mvp * vec4(position, 1.0); }
"#;
        let frag = r#"#version 330 core
in vec2 v_uv;
out vec4 colour;
uniform sampler2D textureArray;
void main() { colour = vec4(v_uv, 0.0, 1.0); }
"#;
        let jar = build_zip(&[
            ("shaders/world/terrain.vsh", vert.as_bytes()),
            ("shaders/world/terrain.fsh", frag.as_bytes()),
        ]);
        let res = port(&jar, &Options::default(), |_, _, _| {}).unwrap();
        assert_eq!(res.pack.count_kind(kind::SHADER), 1);
        assert_eq!(res.report.shader_packs, 1);
        let entry = res.pack.get("shaders/world").expect("pack entry");
        assert_eq!(&entry.data[..4], b"SHD1");
    }

    #[test]
    fn a_shader_pack_that_needs_more_than_the_engine_is_reported() {
        let vert = "#version 330
in vec3 position;
out vec2 v_uv;
void main(){}
";
        let frag = "#version 330
in vec2 v_uv;
out vec4 c;
uniform vec4 shadowMatrix;
void main(){}
";
        let jar = build_zip(&[
            ("shaders/shadow/a.vsh", vert.as_bytes()),
            ("shaders/shadow/a.fsh", frag.as_bytes()),
        ]);
        let res = port(&jar, &Options::default(), |_, _, _| {}).unwrap();
        assert!(
            res.report
                .warnings
                .iter()
                .any(|w| w.contains("shadowMatrix")),
            "{:?}",
            res.report.warnings
        );
    }

    #[test]
    fn report_mentions_coverage() {
        let jar = sample_jar();
        let res = port(&jar, &Options::default(), |_, _, _| {}).unwrap();
        let j = res.report_json();
        assert!(j.contains("\"coverage\""));
        assert!(j.contains("\"minecraft_version\":\"unknown\""));
    }
}
