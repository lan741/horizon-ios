//! The port report — the thing the iPad app shows and the thing users attach
//! to bug reports when a pack does not come over cleanly.

use hz_core::{sha, Json, Pack};

use crate::{EntryClass, Options};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Read,
    BlockStates,
    Models,
    Textures,
    Lang,
    Data,
    Mixins,
    Classes,
    Write,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Read => "read",
            Stage::BlockStates => "blockstates",
            Stage::Models => "models",
            Stage::Textures => "textures",
            Stage::Lang => "lang",
            Stage::Data => "data",
            Stage::Mixins => "mixins",
            Stage::Classes => "classes",
            Stage::Write => "write",
        }
    }
}

fn class_name(c: EntryClass) -> &'static str {
    match c {
        EntryClass::BlockState => "blockstate",
        EntryClass::Model => "model",
        EntryClass::Texture => "texture",
        EntryClass::Lang => "lang",
        EntryClass::Sound => "sound",
        EntryClass::Data => "data",
        EntryClass::Mixin => "mixin",
        EntryClass::McMeta => "mcmeta",
        EntryClass::AnimMeta => "texture_animation",
        EntryClass::Shader => "shader",
        EntryClass::JavaClass => "java_class",
        EntryClass::Other => "other",
    }
}

pub struct Report {
    pub minecraft_version: String,
    /// What the JVM tier still owes: parsed from every `.class` in the jar.
    pub classes: hz_class::ClassSummary,
    pub engine_api: u32,
    pub jar_sha1: String,
    pub jar_sha256: String,
    pub jar_bytes: usize,
    pub jar_entries: usize,
    pub counts: Vec<(EntryClass, u64)>,
    pub java_classes: usize,
    pub class_examples: Vec<String>,
    pub warnings: Vec<String>,
    pub failures: Vec<String>,
    pub coverage: f32,
    pub pack_entries: usize,
    /// Sound events compiled from every `sounds.json` in the jar.
    pub sound_events: usize,
    /// Shader packs compiled from `shaders/` directories.
    pub shader_packs: usize,
}

impl Report {
    pub fn new(opts: &Options, jar: &[u8], zip: &hz_core::zipfile::Zip<'_>) -> Report {
        Report {
            minecraft_version: opts.minecraft_version.clone(),
            classes: hz_class::ClassSummary::default(),
            engine_api: opts.engine_api,
            jar_sha1: sha::sha1_hex(jar),
            jar_sha256: sha::sha256_hex(jar),
            jar_bytes: jar.len(),
            jar_entries: zip.entries().len(),
            counts: Vec::new(),
            java_classes: 0,
            class_examples: Vec::new(),
            warnings: Vec::new(),
            failures: Vec::new(),
            coverage: 0.0,
            pack_entries: 0,
            sound_events: 0,
            shader_packs: 0,
        }
    }

    pub fn count(&mut self, c: EntryClass) {
        match self.counts.iter_mut().find(|(k, _)| *k == c) {
            Some((_, n)) => *n += 1,
            None => self.counts.push((c, 1)),
        }
        if c == EntryClass::JavaClass {
            self.java_classes += 1;
        }
    }

    /// Feed one `.class` body into the transpile inventory. Parse failures are
    /// recorded but never abort a port: the data pipeline is independent.
    pub fn add_class(&mut self, bytes: &[u8], path: &str) {
        match hz_class::parse(bytes) {
            Ok(cf) => self.classes.add(&cf),
            Err(e) => self.warn(format!("{}: {}", path, e)),
        }
    }

    pub fn note_class(&mut self, path: &str) {
        if self.class_examples.len() < 64 {
            self.class_examples.push(path.to_string());
        }
    }

    pub fn warn(&mut self, msg: String) {
        if self.warnings.len() < 200 {
            self.warnings.push(msg);
        }
    }

    pub fn fail(&mut self, msg: String) {
        if self.failures.len() < 200 {
            self.failures.push(msg);
        }
    }

    fn get(&self, c: EntryClass) -> u64 {
        self.counts
            .iter()
            .find(|(k, _)| *k == c)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    }

    /// Coverage = ported data entries / everything that could be data.
    /// Java classes are counted separately: they are not data, and they are
    /// never executed by this pipeline.
    /// Coverage compares what landed in the pack against the data-class
    /// entries the jar actually carried, so an archive we could not read at
    /// all reports 0% rather than a comforting 100%.
    pub fn finish(&mut self, pack: &Pack) {
        let carried = self.get(EntryClass::BlockState)
            + self.get(EntryClass::Model)
            + self.get(EntryClass::Texture)
            + self.get(EntryClass::Lang)
            + self.get(EntryClass::Data)
            + self.get(EntryClass::Sound)
            + self.get(EntryClass::Mixin)
            + self.get(EntryClass::AnimMeta)
            + self.get(EntryClass::Shader);
        let ported = pack
            .entries
            .iter()
            .filter(|e| e.kind != hz_core::kind::MANIFEST)
            .count() as u64;
        self.coverage = if carried == 0 {
            0.0
        } else {
            (ported.min(carried) as f32) / (carried as f32)
        };
    }

    pub fn to_json(&self) -> String {
        let mut counts = Vec::new();
        for (c, n) in &self.counts {
            counts.push((class_name(*c).to_string(), Json::Num(*n as f64)));
        }
        counts.sort_by(|a, b| a.0.cmp(&b.0));

        let obj = Json::Obj(vec![
            ("engine_api".into(), Json::Num(self.engine_api as f64)),
            (
                "minecraft_version".into(),
                Json::Str(self.minecraft_version.clone()),
            ),
            ("jar_sha1".into(), Json::Str(self.jar_sha1.clone())),
            ("jar_sha256".into(), Json::Str(self.jar_sha256.clone())),
            ("jar_bytes".into(), Json::Num(self.jar_bytes as f64)),
            ("jar_entries".into(), Json::Num(self.jar_entries as f64)),
            ("pack_entries".into(), Json::Num(self.pack_entries as f64)),
            (
                "coverage".into(),
                Json::Num(((self.coverage * 1000.0).round() / 1000.0) as f64),
            ),
            ("counts".into(), Json::Obj(counts)),
            ("java_classes".into(), Json::Num(self.java_classes as f64)),
            ("sound_events".into(), Json::Num(self.sound_events as f64)),
            ("shader_packs".into(), Json::Num(self.shader_packs as f64)),
            (
                "jvm".into(),
                Json::parse(&self.classes.to_json()).unwrap_or(Json::Null),
            ),
            (
                "unported".into(),
                Json::Arr(
                    self.class_examples
                        .iter()
                        .map(|s| Json::Str(s.clone()))
                        .collect(),
                ),
            ),
            (
                "warnings".into(),
                Json::Arr(self.warnings.iter().map(|s| Json::Str(s.clone())).collect()),
            ),
            (
                "failures".into(),
                Json::Arr(self.failures.iter().map(|s| Json::Str(s.clone())).collect()),
            ),
        ]);
        obj.to_json_string()
    }

    /// The pack's own manifest entry, which the engine checks before loading.
    pub fn manifest_json(&self, pack: &Pack) -> String {
        let mut kinds = Vec::new();
        for (k, n) in [
            (
                hz_core::kind::BLOCKSTATES,
                pack.count_kind(hz_core::kind::BLOCKSTATES),
            ),
            (hz_core::kind::MODEL, pack.count_kind(hz_core::kind::MODEL)),
            (
                hz_core::kind::TEXTURE,
                pack.count_kind(hz_core::kind::TEXTURE),
            ),
            (hz_core::kind::LANG, pack.count_kind(hz_core::kind::LANG)),
            (hz_core::kind::DATA, pack.count_kind(hz_core::kind::DATA)),
            (hz_core::kind::MIXIN, pack.count_kind(hz_core::kind::MIXIN)),
        ] {
            kinds.push((hz_core::kind_name(k).to_string(), Json::Num(n as f64)));
        }
        let obj = Json::Obj(vec![
            ("format".into(), Json::Str("hzp".into())),
            ("engine_api".into(), Json::Num(self.engine_api as f64)),
            (
                "minecraft_version".into(),
                Json::Str(self.minecraft_version.clone()),
            ),
            ("source_jar_sha1".into(), Json::Str(self.jar_sha1.clone())),
            (
                "source_jar_sha256".into(),
                Json::Str(self.jar_sha256.clone()),
            ),
            (
                "coverage".into(),
                Json::Num(((self.coverage * 1000.0).round() / 1000.0) as f64),
            ),
            ("entries".into(), Json::Obj(kinds)),
            ("java_classes".into(), Json::Num(self.java_classes as f64)),
            ("sound_events".into(), Json::Num(self.sound_events as f64)),
            ("shader_packs".into(), Json::Num(self.shader_packs as f64)),
            (
                "jvm".into(),
                Json::parse(&self.classes.to_json()).unwrap_or(Json::Null),
            ),
            (
                "note".into(),
                Json::Str(
                    "Compiled from a user-supplied Minecraft jar. No Mojang code, textures or \
                     sounds are shipped with the app; assets are produced on device."
                        .into(),
                ),
            ),
        ]);
        obj.to_json_string()
    }
}
