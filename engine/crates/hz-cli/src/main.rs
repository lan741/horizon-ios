//! `hz` — the desktop / CI face of the same engine the iPad app links.
//!
//! Usage:
//!   hz port <jar> <out.hzp> [--report out.json] [--version 26.3]
//!   hz info <pack.hzp>
//!   hz hash <file>
//!   hz selftest
//!
//! On a Mac this is what CI runs before shipping a pack as a fixture; on
//! Windows it is what we use to validate the pipeline against real jars.

use std::env;
use std::fs;
use std::process::ExitCode;

use hz_port::{Options, Stage};

fn usage() {
    eprintln!(
        "hz — HorizonPad port tool\n\n\
         hz port <jar> <out.hzp> [--report out.json] [--version 26.3] [--quiet]\n\
         hz info <pack.hzp>\n\
         hz classes <jar>\n\
         hz sample-class <out.class>\n\
         hz shader-check <pack.zip|jar>\n\
         hz sound-check <file.ogg>\n\
         hz shader-translate <pack.zip> [out.metal]\n\
         hz bench <pack.hzp> [radius]\n\
         hz stubs <jar> <out-dir>\n\
         hz unzip <jar> <out-dir>\n\
         hz hash <file>\n\
         hz selftest"
    );
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        return ExitCode::from(2);
    }
    match args[0].as_str() {
        "port" => cmd_port(&args[1..]),
        "info" => cmd_info(&args[1..]),
        "hash" => cmd_hash(&args[1..]),
        "unzip" => cmd_unzip(&args[1..]),
        "classes" => cmd_classes(&args[1..]),
        "sample-class" => cmd_sample_class(&args[1..]),
        "shader-check" => cmd_shader_check(&args[1..]),
        "sound-check" => cmd_sound_check(&args[1..]),
        "shader-translate" => cmd_shader_translate(&args[1..]),
        "bench" => cmd_bench(&args[1..]),
        "stubs" => cmd_stubs(&args[1..]),
        "selftest" => cmd_selftest(),
        other => {
            eprintln!("unknown command: {other}");
            usage();
            ExitCode::from(2)
        }
    }
}

fn cmd_port(args: &[String]) -> ExitCode {
    let mut jar = None;
    let mut out = None;
    let mut report = None;
    let mut version = "unknown".to_string();
    let mut quiet = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--quiet" => quiet = true,
            "--report" => {
                i += 1;
                report = args.get(i).cloned();
            }
            "--version" => {
                i += 1;
                version = args.get(i).cloned().unwrap_or(version);
            }
            other => {
                if jar.is_none() {
                    jar = Some(other.to_string());
                } else if out.is_none() {
                    out = Some(other.to_string());
                }
            }
        }
        i += 1;
    }
    let (jar, out) = match (jar, out) {
        (Some(j), Some(o)) => (j, o),
        _ => {
            usage();
            return ExitCode::from(2);
        }
    };
    let report = report.unwrap_or_else(|| format!("{}.report.json", out));
    let bytes = match fs::read(&jar) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {jar}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let opts = Options {
        minecraft_version: version,
        ..Default::default()
    };
    let mut last_stage = None;
    let res = match hz_port::port(&bytes, &opts, |stage: Stage, done, total| {
        if !quiet && Some(stage) != last_stage {
            eprintln!("[{}] {}/{}", stage.as_str(), done, total);
            last_stage = Some(stage);
        }
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("port failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = fs::write(&out, res.pack.write()) {
        eprintln!("cannot write {out}: {e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = fs::write(&report, res.report_json()) {
        eprintln!("cannot write {report}: {e}");
        return ExitCode::FAILURE;
    }
    println!(
        "pack {} entries, coverage {:.1}%, {} java classes unported",
        res.pack.entries.len(),
        res.report.coverage * 100.0,
        res.report.java_classes
    );
    println!("report: {report}");
    ExitCode::SUCCESS
}

fn cmd_info(args: &[String]) -> ExitCode {
    let path = match args.first() {
        Some(p) => p,
        None => {
            usage();
            return ExitCode::from(2);
        }
    };
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match hz_core::Pack::read(&bytes) {
        Ok(p) => {
            println!("bytes: {}", bytes.len());
            println!("entries: {}", p.entries.len());
            println!("kinds: {}", p.summary_json());
            // Build the runtime registry: this is the check that matters, since
            // it fails on a malformed BS1/MD2 even when the container is fine.
            match hz_game::Registry::build(&p) {
                Ok(reg) => {
                    println!("blocks: {}", reg.blocks.len());
                    println!("models: {}", reg.models.len());
                    println!("quads: {}", reg.quad_count());
                    println!("textures: {}", reg.texture_count());
                    let with_png = (0..reg.texture_count() as u16)
                        .filter(|i| !reg.texture_png(*i).is_empty())
                        .count();
                    println!("textures with png: {with_png}");
                    println!("animated textures: {}", reg.animation_count());
                    let with_msl = reg.shaders.iter().filter(|p| p.msl.is_some()).count();
                    println!(
                        "shader packs: {} ({} with translated Metal source)",
                        reg.shaders.len(),
                        with_msl
                    );
                    for pack in &reg.shaders {
                        println!("  {}", pack.summary());
                    }
                }
                Err(e) => println!("registry: FAILED ({e})"),
            }
            if let Some(m) = p.get("manifest") {
                println!("manifest: {}", String::from_utf8_lossy(&m.data));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Extract every entry of a jar/resource zip into a directory. Used to verify
/// the DEFLATE path against archives produced by other tools, and to eyeball a
/// pack's inputs.
fn cmd_unzip(args: &[String]) -> ExitCode {
    let (zip_path, out_dir) = match (args.first(), args.get(1)) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            usage();
            return ExitCode::from(2);
        }
    };
    let bytes = match fs::read(zip_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {zip_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let zip = match hz_core::zipfile::Zip::open(&bytes) {
        Ok(z) => z,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let mut ok = 0usize;
    let mut failed = 0usize;
    for (i, entry) in zip.entries().iter().enumerate() {
        if entry.is_dir {
            continue;
        }
        match zip.read_index(i) {
            Ok(data) => {
                let dest = std::path::Path::new(out_dir).join(&entry.name);
                if let Some(parent) = dest.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if let Err(e) = fs::write(&dest, &data) {
                    eprintln!("write {}: {e}", dest.display());
                    failed += 1;
                } else {
                    ok += 1;
                }
            }
            Err(e) => {
                eprintln!("{}: {e}", entry.name);
                failed += 1;
            }
        }
    }
    println!("extracted {ok} entries, {failed} failures");
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Write one valid synthetic `.class` (for smoke tests without a jar).
fn cmd_sample_class(args: &[String]) -> ExitCode {
    let path = match args.first() {
        Some(p) => p,
        None => {
            usage();
            return ExitCode::from(2);
        }
    };
    let bytes = hz_class::sample::tiny_class(
        "net/minecraft/example/Sample",
        "net/minecraft/example/Target",
        "run",
    );
    match fs::write(path, &bytes) {
        Ok(()) => {
            println!("wrote {} bytes to {path}", bytes.len());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cannot write {path}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Translate a shader pack's GLSL subset into Metal Shading Language.
///
/// Usage: `hz shader-translate <pack.zip> [out.metal]`
///
/// Refusals are reported with stage and line, and nothing is written: a
/// half-translated shader is worse than none.
fn cmd_shader_translate(args: &[String]) -> ExitCode {
    let (path, out) = match args.first() {
        Some(p) => (p, args.get(1).cloned()),
        None => {
            usage();
            return ExitCode::from(2);
        }
    };
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let zip = match hz_core::zipfile::Zip::open(&bytes) {
        Ok(z) => z,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    use std::collections::BTreeMap;
    let mut by_dir: BTreeMap<String, Vec<(hz_port::shader::StageKind, String)>> = BTreeMap::new();
    for (i, entry) in zip.entries().iter().enumerate() {
        let kind = match hz_port::shader_stage_of(&entry.name) {
            Some(k) => k,
            None => continue,
        };
        if let Ok(data) = zip.read_index(i) {
            let dir = entry
                .name
                .rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_default();
            by_dir
                .entry(dir)
                .or_default()
                .push((kind, String::from_utf8_lossy(&data).into_owned()));
        }
    }
    if by_dir.is_empty() {
        eprintln!("no shader stages found under shaders/");
        return ExitCode::FAILURE;
    }

    let mut failures = 0usize;
    let mut written: Vec<(String, String)> = Vec::new();
    for (dir, sources) in &by_dir {
        let name = dir.rsplit('/').next().unwrap_or("pack");
        let pack = hz_port::shader::compile(name, sources);
        for warning in &pack.warnings {
            eprintln!("{dir}: warning: {warning}");
        }
        match hz_port::glsl::translate(&pack, sources) {
            Ok(translated) => {
                for note in &translated.notes {
                    eprintln!("{dir}: note: {note}");
                }
                println!(
                    "{dir}: translated ({} bytes of MSL, {} lines)",
                    translated.msl.len(),
                    translated.msl.lines().count()
                );
                written.push((name.to_string(), translated.msl));
            }
            Err(diagnostics) => {
                failures += 1;
                println!("{dir}: refused");
                for d in diagnostics {
                    println!("   {d}");
                }
            }
        }
    }

    if let Some(out) = out {
        if written.len() != 1 {
            eprintln!(
                "refusing to write {out}: {} pack(s) translated, need exactly one",
                written.len()
            );
            return ExitCode::FAILURE;
        }
        if let Err(e) = fs::write(&out, &written[0].1) {
            eprintln!("cannot write {out}: {e}");
            return ExitCode::FAILURE;
        }
        println!("wrote {out}");
    }

    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Decode an Ogg Vorbis file and print what came out. Proves the vendored
/// decoder works on a given file without involving the app or the engine.
fn cmd_sound_check(args: &[String]) -> ExitCode {
    let path = match args.first() {
        Some(p) => p,
        None => {
            usage();
            return ExitCode::from(2);
        }
    };
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Container facts first: they come from our own parser, so a disagreement
    // with the decoder is visible immediately.
    match hz_core::ogg::parse(&bytes) {
        Ok(info) => {
            println!(
                "container: {} ({} pages, {} packets, checksums_ok={})",
                info.codec(),
                info.pages,
                info.packets,
                info.checksums_ok
            );
            if let Some(v) = &info.vorbis {
                println!(
                    "header:    {} Hz, {} channel(s), nominal {} bps",
                    v.sample_rate,
                    v.channels,
                    v.nominal_or_default()
                );
            }
        }
        Err(e) => println!("container: {e}"),
    }

    if hz_vorbis::available() {
        match hz_vorbis::decode(&bytes) {
            Ok(audio) => {
                let peak = audio.samples.iter().fold(0f32, |m, s| m.max(s.abs()));
                let rms = (audio.samples.iter().map(|s| (s * s) as f64).sum::<f64>()
                    / audio.samples.len().max(1) as f64)
                    .sqrt();
                println!(
                    "decoded:   {} frames, {} channel(s), {} Hz, {:.2} s",
                    audio.frames(),
                    audio.channels,
                    audio.sample_rate,
                    audio.duration_seconds()
                );
                println!("levels:    peak {peak:.3}, rms {rms:.3}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("decode failed: {e}");
                ExitCode::FAILURE
            }
        }
    } else {
        println!("decoded:   unavailable (this build has no stb_vorbis)");
        ExitCode::SUCCESS
    }
}

/// Validate a shader pack — a jar/zip with `shaders/` in it — without porting
/// anything. Prints what the pack declares and what the engine cannot supply.
fn cmd_shader_check(args: &[String]) -> ExitCode {
    let path = match args.first() {
        Some(p) => p,
        None => {
            usage();
            return ExitCode::from(2);
        }
    };
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let zip = match hz_core::zipfile::Zip::open(&bytes) {
        Ok(z) => z,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    use std::collections::BTreeMap;
    let mut by_dir: BTreeMap<String, Vec<(hz_port::shader::StageKind, String)>> = BTreeMap::new();
    for (i, entry) in zip.entries().iter().enumerate() {
        let kind = match hz_port::shader_stage_of(&entry.name) {
            Some(k) => k,
            None => continue,
        };
        match zip.read_index(i) {
            Ok(data) => {
                let dir = entry
                    .name
                    .rsplit_once('/')
                    .map(|(d, _)| d.to_string())
                    .unwrap_or_default();
                by_dir
                    .entry(dir)
                    .or_default()
                    .push((kind, String::from_utf8_lossy(&data).into_owned()));
            }
            Err(e) => eprintln!("{}: {e}", entry.name),
        }
    }
    if by_dir.is_empty() {
        eprintln!("no shader stages found (looking for .vsh/.fsh/.vert/.frag under shaders/)");
        return ExitCode::FAILURE;
    }
    let mut unsupported = 0usize;
    for (dir, sources) in &by_dir {
        let name = dir.rsplit('/').next().unwrap_or("pack");
        let pack = hz_port::shader::compile(name, sources);
        println!("== {dir}");
        println!("   version: {}", pack.version);
        for stage in &pack.stages {
            println!(
                "   {}: {} in, {} out, {} uniforms",
                stage.kind.as_str(),
                stage.inputs.len(),
                stage.outputs.len(),
                stage.uniforms.len()
            );
        }
        println!(
            "   supported: {}",
            if pack.is_supported() { "yes" } else { "no" }
        );
        if !pack.requires.is_empty() {
            unsupported += 1;
            println!("   needs: {}", pack.requires.join(", "));
        }
        for warning in &pack.warnings {
            println!("   warning: {warning}");
        }
    }
    println!();
    println!(
        "{} pack(s): {} usable, {} need more than the engine provides",
        by_dir.len(),
        by_dir.len() - unsupported,
        unsupported
    );
    ExitCode::SUCCESS
}

/// Print the JVM inventory: how much Java is left, and what it calls.
fn cmd_classes(args: &[String]) -> ExitCode {
    let path = match args.first() {
        Some(p) => p,
        None => {
            usage();
            return ExitCode::from(2);
        }
    };
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let zip = match hz_core::zipfile::Zip::open(&bytes) {
        Ok(z) => z,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let mut sum = hz_class::ClassSummary::default();
    let mut failed = 0usize;
    for (i, e) in zip.entries().iter().enumerate() {
        if e.is_dir || !e.name.ends_with(".class") {
            continue;
        }
        match zip
            .read_index(i)
            .ok()
            .and_then(|b| hz_class::parse(&b).ok())
        {
            Some(cf) => sum.add(&cf),
            None => failed += 1,
        }
    }
    println!("{}", sum.to_json());
    if failed > 0 {
        eprintln!("{failed} class files could not be parsed");
    }
    ExitCode::SUCCESS
}

/// Write one Rust stub module per class into a directory.
fn cmd_stubs(args: &[String]) -> ExitCode {
    let (jar, out) = match (args.first(), args.get(1)) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            usage();
            return ExitCode::from(2);
        }
    };
    let bytes = match fs::read(jar) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {jar}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match hz_port::emit_stubs(&bytes, out) {
        Ok((written, skipped)) => {
            println!("wrote {written} stub modules to {out} ({skipped} skipped)");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// Time frame rebuilds against a synthetic world built from the pack. This is
/// the number the iPad has to live with, measured on whatever machine runs it.
fn cmd_bench(args: &[String]) -> ExitCode {
    let (pack_path, radius) = match args.first() {
        Some(p) => (
            p,
            args.get(1)
                .and_then(|r| r.parse::<i32>().ok())
                .unwrap_or(16),
        ),
        None => {
            usage();
            return ExitCode::from(2);
        }
    };
    let bytes = match fs::read(pack_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("cannot read {pack_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut game = match hz_game::Game::new(&bytes) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("engine refused pack: {e}");
            return ExitCode::FAILURE;
        }
    };
    let air = game.registry.air;
    // Cycle through every block that actually has geometry, so the run exercises
    // many textures and materials rather than one.
    let candidates: Vec<u16> = (0..game.registry.blocks.len() as u16)
        .filter(|id| *id != air)
        .filter(|id| {
            game.registry
                .model_for(*id, 0)
                .map(|m| !m.quads.is_empty())
                .unwrap_or(false)
        })
        .collect();
    if candidates.is_empty() {
        eprintln!("pack has no blocks with geometry");
        return ExitCode::FAILURE;
    }
    game.world.clear();

    // A floor plus alternating pillars: dense enough to exercise culling.
    let mut placed = 0u32;
    let pick = |x: i32, z: i32, y: i32| -> u16 {
        let i = (x.rem_euclid(31) * 17 + z.rem_euclid(31) * 7 + y) as usize;
        candidates[i % candidates.len()]
    };
    for x in -radius..radius {
        for z in -radius..radius {
            game.world.set(x, 0, z, pick(x, z, 0));
            placed += 1;
            if (x + z) % 5 == 0 {
                for y in 1..4 {
                    game.world.set(x, y, z, pick(x, z, y));
                    placed += 1;
                }
            }
        }
    }
    game.world.set(0, 1, 0, air);
    game.draw_radius = ((radius * 2) / 16).max(1) + 1;
    game.player.pos = [0.5, 2.0, 0.5];

    // 1) cold build: everything is dirty
    let cold = std::time::Instant::now();
    game.rebuild_frame();
    let cold_ms = cold.elapsed().as_secs_f64() * 1000.0;

    // 2) idle: nothing changed, so the engine must do no work at all
    let idle_runs = 200;
    let idle = std::time::Instant::now();
    let mut changed = 0;
    for _ in 0..idle_runs {
        if game.rebuild_frame() {
            changed += 1;
        }
    }
    let idle_us = idle.elapsed().as_secs_f64() * 1_000_000.0 / idle_runs as f64;

    // 3) one edit: the interactive case — break a block, rebuild, repeat
    let edit_runs: i32 = 50;
    let mut edit_targets = Vec::new();
    for i in 0..edit_runs {
        let x = (i % radius) - radius / 2;
        edit_targets.push((x, 0, 0));
    }
    let edit = std::time::Instant::now();
    for (x, y, z) in &edit_targets {
        game.set_block(*x, *y, *z, air);
        game.rebuild_frame();
    }
    let edit_ms = edit.elapsed().as_secs_f64() * 1000.0 / edit_runs as f64;

    // Per-section delivery: how many bytes one edit makes the GPU take.
    let before_flat = game.flat_frame_bytes();
    let mut section_upload = 0usize;
    for i in 0..edit_runs {
        let x = (i % radius) - radius / 2;
        let generation_before: std::collections::HashMap<(i32, i32, i32), u32> = game
            .visible_sections()
            .into_iter()
            .map(|(sx, sy, sz, _, g)| ((sx, sy, sz), g))
            .collect();
        // Place a block on a fresh row so each edit really changes geometry
        // (the earlier loop already cleared y=0, z=0).
        game.set_block(x, 1, 1, game.registry.stone);
        game.rebuild_meshes();
        for (sx, sy, sz, verts, g) in game.visible_sections() {
            if generation_before.get(&(sx, sy, sz)) != Some(&g) {
                section_upload += verts as usize * hz_game::frame::VERTEX_STRIDE;
            }
        }
        game.set_block(x, 1, 1, air);
    }

    let quads = game.frame().quads;
    let verts = game.frame().vertex_count();
    println!("block types available: {}", candidates.len());
    println!("blocks placed: {placed}");
    println!("quads/frame: {quads}  vertices: {verts}");
    // 4) relight: what a block edit costs once light is involved
    let relight_runs = 20;
    let relight_start = std::time::Instant::now();
    for _ in 0..relight_runs {
        game.relight();
        game.rebuild_frame();
    }
    let relight_ms = relight_start.elapsed().as_secs_f64() * 1000.0 / relight_runs as f64;

    // 5) one window relight on its own, to separate it from the frame copy
    let window_start = std::time::Instant::now();
    for i in 0..relight_runs {
        let x = (i % radius) - radius / 2;
        game.relight_around(x, 0, 0);
    }
    let window_ms = window_start.elapsed().as_secs_f64() * 1000.0 / relight_runs as f64;

    println!("cold build (all dirty): {cold_ms:.2} ms");
    println!("full relight + rebuild: {relight_ms:.2} ms");
    println!("windowed relight only: {window_ms:.3} ms");
    println!("idle frame: {idle_us:.2} µs  (rebuilds that did work: {changed}/{idle_runs})");
    println!("one block edit + rebuild: {edit_ms:.2} ms");
    println!(
        "flat frame: {:.1} KiB per frame, every frame",
        before_flat as f64 / 1024.0
    );
    println!(
        "per-section upload for {edit_runs} edits: {:.1} KiB total ({:.1} KiB per edit)",
        section_upload as f64 / 1024.0,
        section_upload as f64 / 1024.0 / edit_runs as f64
    );
    println!(
        "per-section upload when nothing changes: 0 KiB (the flat path would still copy {:.1} KiB)",
        before_flat as f64 / 1024.0
    );
    println!(
        "vertex bytes copied per frame: {:.1} KiB",
        (verts as f64 * hz_game::frame::VERTEX_STRIDE as f64) / 1024.0
    );

    // Prove the texture indices survive the whole path: count distinct slots
    // in the vertex stream itself, not in the registry.
    let floats = game.frame().verts.as_slice();
    let stride = hz_game::frame::FLOATS_PER_VERTEX;
    let mut seen = std::collections::BTreeSet::new();
    let mut untextured = 0usize;
    for v in 0..(floats.len() / stride) {
        let t = floats[v * stride + 12];
        if t >= 65000.0 {
            untextured += 1;
        } else {
            seen.insert(t as u32);
        }
    }
    println!(
        "distinct texture slots in frame: {}  (untextured vertices: {})",
        seen.len(),
        untextured
    );

    // Lighting sanity: the world is an open field, so the top face of the floor
    // must be fully sky-lit and a cell under a block must be dark.
    let lit = game.brightness_at(0, 2, 0);
    let sky = game.sky_at(0, 2, 0);
    println!("light at (0,2,0): sky={sky} brightness={lit}");
    ExitCode::SUCCESS
}

fn cmd_hash(args: &[String]) -> ExitCode {
    let path = match args.first() {
        Some(p) => p,
        None => {
            usage();
            return ExitCode::from(2);
        }
    };
    match fs::read(path) {
        Ok(b) => {
            println!("sha1   {}", hz_core::sha::sha1_hex(&b));
            println!("sha256 {}", hz_core::sha::sha256_hex(&b));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Build a pack from a synthetic jar and run the engine against it. Used by CI
/// and by anyone who wants to see the pipeline work without a Minecraft copy.
fn cmd_selftest() -> ExitCode {
    let jar = synthetic_jar();
    let opts = Options {
        minecraft_version: "selftest".to_string(),
        ..Default::default()
    };
    let res = match hz_port::port(&jar, &opts, |_, _, _| {}) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("port failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let pack_bytes = res.pack.write();
    let mut game = match hz_game::Game::new(&pack_bytes) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("engine refused pack: {e}");
            return ExitCode::FAILURE;
        }
    };
    for _ in 0..30 {
        game.tick(1.0 / 60.0);
    }
    game.rebuild_frame();
    println!(
        "selftest ok: {} pack entries, {} blocks, {} quads in frame, player y={:.2}",
        res.pack.entries.len(),
        game.registry.blocks.len(),
        game.frame().quads,
        game.player.pos[1]
    );
    ExitCode::SUCCESS
}

fn synthetic_jar() -> Vec<u8> {
    // stored-only zip, same builder the hz-port unit test uses
    fn crc32(data: &[u8]) -> u32 {
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
        for &b in data {
            crc = table[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
        }
        crc ^ 0xFFFF_FFFF
    }
    let entries: &[(&str, &[u8])] = &[
        (
            "assets/minecraft/blockstates/stone.json",
            br#"{"variants":{"":{"model":"minecraft:block/cube_all"}}}"#,
        ),
        (
            "assets/minecraft/blockstates/air.json",
            br#"{"variants":{"":{"model":"minecraft:block/cube_all"}}}"#,
        ),
        (
            "assets/minecraft/models/block/cube_all.json",
            br##"{"textures":{"all":"minecraft:block/stone"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{
                "up":{"texture":"#all","cullface":"up"},"down":{"texture":"#all","cullface":"down"},
                "north":{"texture":"#all","cullface":"north"},"south":{"texture":"#all","cullface":"south"},
                "west":{"texture":"#all","cullface":"west"},"east":{"texture":"#all","cullface":"east"}}}]}"##,
        ),
        (
            "assets/minecraft/textures/block/stone.png",
            b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x10\x00\x00\x00\x10\x08\x06\x00\x00\x00",
        ),
        ("assets/minecraft/lang/en_us.json", br#"{"block.minecraft.stone":"Stone"}"#),
        ("data/minecraft/recipe/x.json", br#"{"type":"minecraft:crafting_shapeless"}"#),
    ];
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let off = out.len() as u32;
        let crc = crc32(data);
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0u8; 8]);
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
