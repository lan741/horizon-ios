//! hz-ffi — the C ABI the Swift app links against.
//!
//! Rules that keep this layer boring and safe to call from Swift:
//! * every function takes/returns C types only;
//! * every fallible function returns an `i32` status and leaves the message in
//!   a thread-local buffer you read with [`hz_last_error`];
//! * strings returned to Swift are owned allocations and must be released with
//!   [`hz_string_free`];
//! * opaque handles are heap boxes, null-checked on entry, and freed exactly
//!   once by the caller.
//!
//! Nothing here spawns processes, opens sockets, or writes outside the paths
//! the host app hands us — App Store review depends on that.
//!
//! # Safety contract for every `unsafe extern "C"` function below
//!
//! * Pointer arguments must be valid, NUL-terminated UTF-8, or null where the
//!   signature documents a null fallback.
//! * Output buffers must be sized as the parameter says; a too-small buffer is
//!   rejected with a 0 return rather than written past.
//! * Handles must come from the matching constructor and be freed once.
//!
//! Individual violations are memory-unsafe, which is why each function is
//! `unsafe`; the checks above are what the implementation actually performs.

#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::ffi::{c_char, c_void, CStr, CString};
use std::fs;
use std::path::Path;

use hz_game::Game;
use hz_port::{Options, Stage};
use hz_vorbis::Decoded;

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::new("").unwrap());
}

fn set_error(msg: impl Into<String>) {
    let s = msg.into().replace('\0', " ");
    LAST_ERROR.with(|c| {
        *c.borrow_mut() = CString::new(s).unwrap_or_else(|_| CString::new("error").unwrap())
    });
}

unsafe fn cstr<'a>(p: *const c_char) -> Result<&'a str, String> {
    if p.is_null() {
        return Err("null string pointer".to_string());
    }
    CStr::from_ptr(p)
        .to_str()
        .map_err(|e| format!("path is not valid UTF-8: {}", e))
}

pub type HzProgress = extern "C" fn(ctx: *mut c_void, stage: *const c_char, done: u32, total: u32);

/// ABI version; the Swift side refuses packs from a different major.
#[no_mangle]
pub extern "C" fn hz_api_version() -> u32 {
    1
}

#[no_mangle]
pub extern "C" fn hz_last_error() -> *const c_char {
    LAST_ERROR.with(|c| c.borrow().as_ptr())
}

/// Run the whole port pipeline: `jar` (a user-supplied client jar or resource
/// zip) → `out_hzp` plus a JSON report at `out_report`.
#[no_mangle]
pub unsafe extern "C" fn hz_port_jar(
    jar: *const c_char,
    out_hzp: *const c_char,
    out_report: *const c_char,
    minecraft_version: *const c_char,
    cb: Option<HzProgress>,
    ctx: *mut c_void,
) -> i32 {
    let run = || -> Result<(), String> {
        let jar_path = cstr(jar)?;
        let hzp_path = cstr(out_hzp)?;
        let report_path = cstr(out_report)?;
        let version = if minecraft_version.is_null() {
            "unknown".to_string()
        } else {
            cstr(minecraft_version)?.to_string()
        };

        let bytes = fs::read(jar_path).map_err(|e| format!("cannot read {}: {}", jar_path, e))?;
        let opts = Options {
            minecraft_version: version,
            ..Default::default()
        };

        let stage_names: Vec<String> = [
            Stage::Read,
            Stage::BlockStates,
            Stage::Models,
            Stage::Textures,
            Stage::Lang,
            Stage::Data,
            Stage::Mixins,
            Stage::Classes,
            Stage::Write,
        ]
        .iter()
        .map(|s| s.as_str().to_string())
        .collect();

        let result = hz_port::port(&bytes, &opts, |stage, done, total| {
            if let Some(f) = cb {
                let name = stage_names[stage as usize].clone();
                if let Ok(cs) = CString::new(name) {
                    f(ctx, cs.as_ptr(), done, total);
                }
            }
        })?;

        if let Some(dir) = Path::new(hzp_path).parent() {
            fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
        }
        fs::write(hzp_path, result.pack.write())
            .map_err(|e| format!("cannot write {}: {}", hzp_path, e))?;
        fs::write(report_path, result.report_json())
            .map_err(|e| format!("cannot write {}: {}", report_path, e))?;
        Ok(())
    };

    match run() {
        Ok(()) => 0,
        Err(e) => {
            set_error(e);
            1
        }
    }
}

/// JSON summary of a pack: entry counts, manifest hash, size.
#[no_mangle]
pub unsafe extern "C" fn hz_pack_summary(pack_path: *const c_char) -> *mut c_char {
    let run = || -> Result<String, String> {
        let p = cstr(pack_path)?;
        let bytes = fs::read(p).map_err(|e| format!("cannot read {}: {}", p, e))?;
        let pack = hz_core::Pack::read(&bytes)?;
        let manifest_sha = pack
            .get("manifest")
            .map(|e| hz_core::sha::sha256_hex(&e.data))
            .unwrap_or_else(|| "none".to_string());
        Ok(format!(
            "{{\"entries\":{},\"kinds\":{},\"manifest_sha256\":\"{}\",\"bytes\":{}}}",
            pack.entries.len(),
            pack.summary_json(),
            manifest_sha,
            bytes.len()
        ))
    };
    match run() {
        Ok(s) => CString::new(s)
            .map(|c| c.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        Err(e) => {
            set_error(e);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn hz_string_free(p: *mut c_char) {
    if !p.is_null() {
        drop(CString::from_raw(p));
    }
}

// ---------------------------------------------------------------- game handle

pub struct HzGame {
    game: Game,
}

/// Open a compiled pack and start a session. Null on error.
#[no_mangle]
pub unsafe extern "C" fn hz_game_new(pack_path: *const c_char) -> *mut HzGame {
    let run = || -> Result<HzGame, String> {
        let p = cstr(pack_path)?;
        let bytes = fs::read(p).map_err(|e| format!("cannot read {}: {}", p, e))?;
        let game = Game::new(&bytes)?;
        Ok(HzGame { game })
    };
    match run() {
        Ok(g) => Box::into_raw(Box::new(g)),
        Err(e) => {
            set_error(e);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn hz_game_free(g: *mut HzGame) {
    if !g.is_null() {
        drop(Box::from_raw(g));
    }
}

#[no_mangle]
pub unsafe extern "C" fn hz_game_tick(g: *mut HzGame, dt_seconds: f32) {
    if let Some(h) = g.as_mut() {
        h.game.tick(dt_seconds);
    }
}

/// Re-bake dirty sections **without** rebuilding the flat frame.
///
/// This is the path a renderer with per-section GPU buffers wants: it never
/// copies the megabyte-sized contiguous buffer, it only asks which sections
/// moved. Returns the current revision, or 0 when nothing changed.
#[no_mangle]
pub unsafe extern "C" fn hz_game_rebuild_meshes(g: *mut HzGame) -> u32 {
    match g.as_mut() {
        Some(h) => h.game.rebuild_meshes(),
        None => 0,
    }
}

/// Number of sections the renderer should draw right now.
#[no_mangle]
pub unsafe extern "C" fn hz_game_section_count(g: *const HzGame) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.visible_sections().len() as u32,
        None => 0,
    }
}

/// Describe section `index`: `out5 = x, y, z, vertex_count, generation`.
/// Returns 5, or 0 when the index is out of range.
#[no_mangle]
pub unsafe extern "C" fn hz_game_section_at(g: *const HzGame, index: u32, out5: *mut u32) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    if out5.is_null() {
        return 0;
    }
    let sections = h.game.visible_sections();
    let entry = match sections.get(index as usize) {
        Some(e) => e,
        None => return 0,
    };
    *out5 = entry.0 as u32;
    *out5.add(1) = entry.1 as u32;
    *out5.add(2) = entry.2 as u32;
    *out5.add(3) = entry.3;
    *out5.add(4) = entry.4;
    5
}

/// Copy section `(x, y, z)`'s interleaved vertices into `out`.
/// Returns floats written; 0 when the section is absent or the buffer is small.
#[no_mangle]
pub unsafe extern "C" fn hz_game_section_vertices(
    g: *const HzGame,
    x: i32,
    y: i32,
    z: i32,
    out: *mut f32,
    cap_floats: u32,
) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    let data = match h.game.section_vertices((x, y, z)) {
        Some(d) => d,
        None => return 0,
    };
    if out.is_null() || data.len() > cap_floats as usize {
        return 0;
    }
    std::ptr::copy_nonoverlapping(data.as_ptr(), out, data.len());
    data.len() as u32
}

/// Bytes the flat assembler would copy for the current view: the number the
/// per-section path avoids re-copying every frame.
#[no_mangle]
pub unsafe extern "C" fn hz_game_flat_frame_bytes(g: *const HzGame) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.flat_frame_bytes() as u32,
        None => 0,
    }
}

/// Re-bake dirty sections and reassemble the frame.
/// Returns 1 when the vertex buffer changed, 0 when nothing moved (an idle
/// frame costs one comparison, so callers can skip the copy entirely).
#[no_mangle]
pub unsafe extern "C" fn hz_game_rebuild_frame(g: *mut HzGame) -> u32 {
    match g.as_mut() {
        Some(h) => u32::from(h.game.rebuild_frame()),
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn hz_game_frame_quads(g: *const HzGame) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.frame().quads,
        None => 0,
    }
}

/// Copy the interleaved vertex buffer into `out` (floats). Returns floats
/// written, which is `0` when the caller's capacity is too small.
#[no_mangle]
pub unsafe extern "C" fn hz_game_frame_data(
    g: *const HzGame,
    out: *mut f32,
    cap_floats: u32,
) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    let f = h.game.frame();
    if out.is_null() || f.verts.len() > cap_floats as usize {
        return 0;
    }
    std::ptr::copy_nonoverlapping(f.verts.as_ptr(), out, f.verts.len());
    f.verts.len() as u32
}

#[no_mangle]
pub unsafe extern "C" fn hz_game_set_player(
    g: *mut HzGame,
    x: f32,
    y: f32,
    z: f32,
    yaw: f32,
    pitch: f32,
) {
    if let Some(h) = g.as_mut() {
        h.game.player.pos = [x, y, z];
        h.game.player.yaw = yaw;
        h.game.player.pitch = pitch;
        h.game.player.vel = [0.0; 3];
    }
}

#[no_mangle]
pub unsafe extern "C" fn hz_game_player_pos(g: *const HzGame, out: *mut f32) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    if out.is_null() {
        return 0;
    }
    let p = h.game.player.pos;
    *out = p[0];
    *out.add(1) = p[1];
    *out.add(2) = p[2];
    3
}

#[no_mangle]
pub unsafe extern "C" fn hz_game_set_block(g: *mut HzGame, x: i32, y: i32, z: i32, id: u32) {
    if let Some(h) = g.as_mut() {
        h.game.set_block(x, y, z, id as u16);
    }
}

#[no_mangle]
pub unsafe extern "C" fn hz_game_stats(g: *const HzGame, out: *mut u32) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    if out.is_null() {
        return 0;
    }
    *out = h.game.ticks as u32;
    *out.add(1) = h.game.frames as u32;
    *out.add(2) = h.game.frame().quads;
    *out.add(3) = h.game.world.non_air();
    4
}

/// Voxel raycast. `out` receives 7 floats:
/// `block.x block.y block.z  normal.x normal.y normal.z  distance`.
/// Returns 7 on a hit, 0 on a miss or bad arguments.
#[no_mangle]
pub unsafe extern "C" fn hz_game_raycast(
    g: *const HzGame,
    ox: f32,
    oy: f32,
    oz: f32,
    dx: f32,
    dy: f32,
    dz: f32,
    max_distance: f32,
    out: *mut f32,
) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    if out.is_null() {
        return 0;
    }
    match h.game.raycast([ox, oy, oz], [dx, dy, dz], max_distance) {
        None => 0,
        Some(hit) => {
            *out = hit.block.0 as f32;
            *out.add(1) = hit.block.1 as f32;
            *out.add(2) = hit.block.2 as f32;
            *out.add(3) = hit.normal.0 as f32;
            *out.add(4) = hit.normal.1 as f32;
            *out.add(5) = hit.normal.2 as f32;
            *out.add(6) = hit.distance;
            7
        }
    }
}

/// Id of air for this pack, so the caller never hardcodes 0.
#[no_mangle]
pub unsafe extern "C" fn hz_game_air_id(g: *const HzGame) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.air() as u32,
        None => 0,
    }
}

/// Resolve a block name to its dense id. Returns `u32::MAX` when the pack does
/// not contain that block.
#[no_mangle]
pub unsafe extern "C" fn hz_game_block_id(g: *const HzGame, name: *const c_char) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return u32::MAX,
    };
    let name = match cstr(name) {
        Ok(n) => n,
        Err(_) => return u32::MAX,
    };
    match h.game.registry.id(name) {
        Some(id) => id as u32,
        None => u32::MAX,
    }
}

/// Number of blocks in the pack, so a picker can walk them by index.
#[no_mangle]
pub unsafe extern "C" fn hz_game_block_count(g: *const HzGame) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.registry.blocks.len() as u32,
        None => 0,
    }
}

/// Copy the name of block `index` into `out`. Returns bytes written.
#[no_mangle]
pub unsafe extern "C" fn hz_game_block_name(
    g: *const HzGame,
    index: u32,
    out: *mut c_char,
    cap: u32,
) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    let name = match h.game.registry.blocks.get(index as usize) {
        Some(b) => b.name.as_str(),
        None => return 0,
    };
    if out.is_null() || cap == 0 {
        return 0;
    }
    let bytes = name.as_bytes();
    let n = bytes.len().min(cap as usize - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, n);
    *out.add(n) = 0;
    n as u32
}

/// Number of textures in the pack's global table.
#[no_mangle]
pub unsafe extern "C" fn hz_game_texture_count(g: *const HzGame) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.registry.texture_count() as u32,
        None => 0,
    }
}

/// Copy texture `index`'s id (`minecraft:block/stone`) into `out`.
/// Returns bytes written (excluding the NUL) or 0.
#[no_mangle]
pub unsafe extern "C" fn hz_game_texture_name(
    g: *const HzGame,
    index: u32,
    out: *mut c_char,
    cap: u32,
) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    let name = match h.game.registry.texture_ids.get(index as usize) {
        Some(n) => n,
        None => return 0,
    };
    if out.is_null() || cap == 0 {
        return 0;
    }
    let bytes = name.as_bytes();
    let n = bytes.len().min(cap as usize - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, n);
    *out.add(n) = 0;
    n as u32
}

/// Byte length of texture `index`'s PNG payload.
#[no_mangle]
pub unsafe extern "C" fn hz_game_texture_png_len(g: *const HzGame, index: u32) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.registry.texture_png(index as u16).len() as u32,
        None => 0,
    }
}

/// Copy texture `index`'s PNG payload into `out`. Returns bytes written; 0
/// when the slot is empty or the buffer is too small.
#[no_mangle]
pub unsafe extern "C" fn hz_game_texture_png(
    g: *const HzGame,
    index: u32,
    out: *mut u8,
    cap: u32,
) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    let png = h.game.registry.texture_png(index as u16);
    if out.is_null() || png.len() > cap as usize {
        return 0;
    }
    std::ptr::copy_nonoverlapping(png.as_ptr(), out, png.len());
    png.len() as u32
}

/// Daylight multiplier for the sky channel: 0 = night, 1 = noon.
/// Re-bakes the affected sections on the next `hz_game_rebuild_frame`.
#[no_mangle]
pub unsafe extern "C" fn hz_game_set_daylight(g: *mut HzGame, daylight: f32) {
    if let Some(h) = g.as_mut() {
        h.game.set_daylight(daylight);
    }
}

#[no_mangle]
pub unsafe extern "C" fn hz_game_daylight(g: *const HzGame) -> f32 {
    match g.as_ref() {
        Some(h) => h.game.daylight(),
        None => 1.0,
    }
}

/// Raw sky channel 0..15 at a block position.
#[no_mangle]
pub unsafe extern "C" fn hz_game_sky_at(g: *const HzGame, x: i32, y: i32, z: i32) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.sky_at(x, y, z) as u32,
        None => 0,
    }
}

/// Raw block-light channel 0..15 at a block position.
#[no_mangle]
pub unsafe extern "C" fn hz_game_block_light_at(g: *const HzGame, x: i32, y: i32, z: i32) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.light_block_at(x, y, z) as u32,
        None => 0,
    }
}

/// Combined brightness 0..15 after daylight, i.e. what the mesher bakes.
#[no_mangle]
pub unsafe extern "C" fn hz_game_brightness_at(g: *const HzGame, x: i32, y: i32, z: i32) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.brightness_at(x, y, z) as u32,
        None => 0,
    }
}

// ------------------------------------------------------------------- sounds

/// Number of sound events the pack defines.
#[no_mangle]
pub unsafe extern "C" fn hz_game_sound_event_count(g: *const HzGame) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.sound_event_count() as u32,
        None => 0,
    }
}

/// Number of sound payloads (Ogg files) in the pack.
#[no_mangle]
pub unsafe extern "C" fn hz_game_sound_asset_count(g: *const HzGame) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.registry.sounds.asset_count() as u32,
        None => 0,
    }
}

/// Copy sound asset `index`'s id (`minecraft:dig/stone1`) into `out`.
#[no_mangle]
pub unsafe extern "C" fn hz_game_sound_asset_id(
    g: *const HzGame,
    index: u32,
    out: *mut c_char,
    cap: u32,
) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    let id = match h.game.registry.sounds.asset(index as u16) {
        Some(a) => &a.id,
        None => return 0,
    };
    if out.is_null() || cap == 0 {
        return 0;
    }
    let bytes = id.as_bytes();
    let n = bytes.len().min(cap as usize - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, n);
    *out.add(n) = 0;
    n as u32
}

/// Byte length of sound asset `index`'s payload.
#[no_mangle]
pub unsafe extern "C" fn hz_game_sound_len(g: *const HzGame, index: u32) -> u32 {
    match g.as_ref() {
        Some(h) => h
            .game
            .registry
            .sounds
            .asset(index as u16)
            .map(|a| a.bytes.len() as u32)
            .unwrap_or(0),
        None => 0,
    }
}

/// Copy sound asset `index`'s bytes (an Ogg container) into `out`.
#[no_mangle]
pub unsafe extern "C" fn hz_game_sound_data(
    g: *const HzGame,
    index: u32,
    out: *mut u8,
    cap: u32,
) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    let asset = match h.game.registry.sounds.asset(index as u16) {
        Some(a) => a,
        None => return 0,
    };
    if out.is_null() || asset.bytes.len() > cap as usize {
        return 0;
    }
    std::ptr::copy_nonoverlapping(asset.bytes.as_ptr(), out, asset.bytes.len());
    asset.bytes.len() as u32
}

/// Container facts for sound asset `index`: `out4 = samples_per_second,
/// channels, duration_milliseconds, checksums_ok`. Returns 4, or 0 when the
/// asset is absent. `samples_per_second == 0` means "not a Vorbis stream".
#[no_mangle]
pub unsafe extern "C" fn hz_game_sound_info(g: *const HzGame, index: u32, out: *mut u32) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    let asset = match h.game.registry.sounds.asset(index as u16) {
        Some(a) => a,
        None => return 0,
    };
    if out.is_null() {
        return 0;
    }
    let rate = asset
        .info
        .vorbis
        .as_ref()
        .map(|v| v.sample_rate)
        .unwrap_or(0);
    let channels = asset
        .info
        .vorbis
        .as_ref()
        .map(|v| v.channels as u32)
        .unwrap_or(0);
    *out = rate;
    *out.add(1) = channels;
    *out.add(2) = (asset.info.duration_seconds() * 1000.0) as u32;
    *out.add(3) = u32::from(asset.info.checksums_ok);
    4
}

/// Queue a sound event at a world position. Returns 1 when something was
/// queued (in range, known event, payload present).
#[no_mangle]
pub unsafe extern "C" fn hz_game_play_sound(
    g: *mut HzGame,
    event: *const c_char,
    x: f32,
    y: f32,
    z: f32,
) -> u32 {
    let h = match g.as_mut() {
        Some(h) => h,
        None => return 0,
    };
    let name = match cstr(event) {
        Ok(n) => n,
        Err(_) => return 0,
    };
    u32::from(h.game.play_sound(name, x, y, z))
}

/// Pop the next queued sound: `out3 = asset_index, volume, pitch`.
/// Returns 1 when a request was written, 0 when the queue is empty.
#[no_mangle]
pub unsafe extern "C" fn hz_game_next_sound(g: *mut HzGame, out: *mut f32) -> u32 {
    let h = match g.as_mut() {
        Some(h) => h,
        None => return 0,
    };
    if out.is_null() {
        return 0;
    }
    match h.game.next_sound() {
        Some(request) => {
            *out = request.asset as f32;
            *out.add(1) = request.volume;
            *out.add(2) = request.pitch;
            1
        }
        None => 0,
    }
}

// -------------------------------------------------------------- ogg vorbis

/// Decoded audio owned by the caller until `hz_pcm_free`.
pub struct HzPcm {
    audio: Decoded,
}

/// 1 when this build links the vendored Vorbis decoder.
#[no_mangle]
pub extern "C" fn hz_vorbis_available() -> u32 {
    u32::from(hz_vorbis::available())
}

/// Decode a whole Ogg Vorbis file. Returns null on failure and leaves the
/// reason in `hz_last_error`. Callers own the result and must free it.
#[no_mangle]
pub unsafe extern "C" fn hz_pcm_decode(data: *const u8, len: u32) -> *mut HzPcm {
    if data.is_null() || len == 0 {
        set_error("hz_pcm_decode: empty input");
        return std::ptr::null_mut();
    }
    let bytes = std::slice::from_raw_parts(data, len as usize);
    match hz_vorbis::decode(bytes) {
        Ok(audio) => Box::into_raw(Box::new(HzPcm { audio })),
        Err(e) => {
            set_error(format!("hz_pcm_decode: {e}"));
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn hz_pcm_channels(pcm: *const HzPcm) -> u32 {
    pcm.as_ref().map(|p| p.audio.channels as u32).unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn hz_pcm_rate(pcm: *const HzPcm) -> u32 {
    pcm.as_ref().map(|p| p.audio.sample_rate).unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn hz_pcm_frames(pcm: *const HzPcm) -> u32 {
    pcm.as_ref()
        .map(|p| p.audio.frames().min(u32::MAX as usize) as u32)
        .unwrap_or(0)
}

/// Copy interleaved f32 samples (`frames * channels` of them) into `out`.
/// Returns the number written; 0 when the buffer is too small.
#[no_mangle]
pub unsafe extern "C" fn hz_pcm_data(pcm: *const HzPcm, out: *mut f32, cap_floats: u32) -> u32 {
    let pcm = match pcm.as_ref() {
        Some(p) => p,
        None => return 0,
    };
    if out.is_null() || pcm.audio.samples.len() > cap_floats as usize {
        return 0;
    }
    std::ptr::copy_nonoverlapping(pcm.audio.samples.as_ptr(), out, pcm.audio.samples.len());
    pcm.audio.samples.len() as u32
}

#[no_mangle]
pub unsafe extern "C" fn hz_pcm_free(pcm: *mut HzPcm) {
    if !pcm.is_null() {
        drop(Box::from_raw(pcm));
    }
}

// ------------------------------------------------------------------ shaders

/// Number of shader packs in the pack.
#[no_mangle]
pub unsafe extern "C" fn hz_game_shader_count(g: *const HzGame) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.registry.shaders.len() as u32,
        None => 0,
    }
}

/// Copy shader pack `index`'s name into `out`.
#[no_mangle]
pub unsafe extern "C" fn hz_game_shader_name(
    g: *const HzGame,
    index: u32,
    out: *mut c_char,
    cap: u32,
) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    let name = match h.game.registry.shaders.get(index as usize) {
        Some(s) => &s.name,
        None => return 0,
    };
    if out.is_null() || cap == 0 {
        return 0;
    }
    let bytes = name.as_bytes();
    let n = bytes.len().min(cap as usize - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, n);
    *out.add(n) = 0;
    n as u32
}

/// Copy the pack's one-line summary ("world — vertex+fragment, needs ...").
#[no_mangle]
pub unsafe extern "C" fn hz_game_shader_summary(
    g: *const HzGame,
    index: u32,
    out: *mut c_char,
    cap: u32,
) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    let summary = match h.game.registry.shaders.get(index as usize) {
        Some(s) => s.summary(),
        None => return 0,
    };
    if out.is_null() || cap == 0 {
        return 0;
    }
    let bytes = summary.as_bytes();
    let n = bytes.len().min(cap as usize - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, n);
    *out.add(n) = 0;
    n as u32
}

/// Byte length of pack `index`'s translated Metal source; 0 when there is none
/// (the pack was refused, or it had no shaders).
#[no_mangle]
pub unsafe extern "C" fn hz_game_shader_msl_len(g: *const HzGame, index: u32) -> u32 {
    match g.as_ref() {
        Some(h) => h
            .game
            .registry
            .shaders
            .get(index as usize)
            .and_then(|s| s.msl.as_ref())
            .map(|m| m.len() as u32)
            .unwrap_or(0),
        None => 0,
    }
}

/// Copy pack `index`'s translated Metal source into `out` (UTF-8, not
/// NUL-terminated). Returns bytes written; 0 when there is none or the buffer is
/// too small.
#[no_mangle]
pub unsafe extern "C" fn hz_game_shader_msl(
    g: *const HzGame,
    index: u32,
    out: *mut u8,
    cap: u32,
) -> u32 {
    let h = match g.as_ref() {
        Some(h) => h,
        None => return 0,
    };
    let msl = match h
        .game
        .registry
        .shaders
        .get(index as usize)
        .and_then(|s| s.msl.as_ref())
    {
        Some(m) => m,
        None => return 0,
    };
    if out.is_null() || msl.len() > cap as usize {
        return 0;
    }
    std::ptr::copy_nonoverlapping(msl.as_ptr(), out, msl.len());
    msl.len() as u32
}

/// 1 when the engine could drive pack `index`'s interface as-is.
#[no_mangle]
pub unsafe extern "C" fn hz_game_shader_supported(g: *const HzGame, index: u32) -> u32 {
    match g.as_ref() {
        Some(h) => h
            .game
            .registry
            .shaders
            .get(index as usize)
            .map(|s| u32::from(s.supported))
            .unwrap_or(0),
        None => 0,
    }
}

/// Total number of shader packs the engine could drive.
#[no_mangle]
pub unsafe extern "C" fn hz_game_supported_shader_count(g: *const HzGame) -> u32 {
    match g.as_ref() {
        Some(h) => h.game.registry.supported_shader_count() as u32,
        None => 0,
    }
}

/// Persist the current world and player to a `.hzw` file.
#[no_mangle]
pub unsafe extern "C" fn hz_game_save(g: *const HzGame, path: *const c_char) -> i32 {
    let run = || -> Result<(), String> {
        let h = g.as_ref().ok_or("null game handle")?;
        let p = cstr(path)?;
        h.game.save(p)
    };
    match run() {
        Ok(()) => 0,
        Err(e) => {
            set_error(e);
            1
        }
    }
}

/// Load a `.hzw` file into a live handle. The pack it was built from must be
/// the one the handle was opened with; the app checks the manifest first.
#[no_mangle]
pub unsafe extern "C" fn hz_game_load(g: *mut HzGame, path: *const c_char) -> i32 {
    let run = || -> Result<(), String> {
        let h = g.as_mut().ok_or("null game handle")?;
        let p = cstr(path)?;
        h.game.load(p)
    };
    match run() {
        Ok(()) => 0,
        Err(e) => {
            set_error(e);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_buffer_is_set() {
        unsafe {
            let status = hz_port_jar(
                c"/nonexistent.jar".as_ptr(),
                c"/tmp/x.hzp".as_ptr(),
                c"/tmp/x.json".as_ptr(),
                std::ptr::null(),
                None,
                std::ptr::null_mut(),
            );
            assert_eq!(status, 1);
            let msg = CStr::from_ptr(hz_last_error())
                .to_string_lossy()
                .into_owned();
            assert!(msg.contains("cannot read"), "msg={msg}");
        }
    }

    #[test]
    fn string_free_accepts_null() {
        unsafe { hz_string_free(std::ptr::null_mut()) };
    }

    #[test]
    fn pcm_decode_reports_failures_and_refuses_nulls() {
        unsafe {
            assert!(hz_pcm_decode(std::ptr::null(), 0).is_null());
            let msg = CStr::from_ptr(hz_last_error())
                .to_string_lossy()
                .into_owned();
            assert!(msg.contains("empty input"), "msg={msg}");

            let garbage = b"not an ogg stream at all";
            assert!(hz_pcm_decode(garbage.as_ptr(), garbage.len() as u32).is_null());

            // The accessors must tolerate null handles.
            assert_eq!(hz_pcm_channels(std::ptr::null()), 0);
            assert_eq!(hz_pcm_rate(std::ptr::null()), 0);
            assert_eq!(hz_pcm_frames(std::ptr::null()), 0);
            hz_pcm_free(std::ptr::null_mut());
        }
    }

    #[test]
    fn api_version_is_stable() {
        assert_eq!(hz_api_version(), 1);
    }

    #[test]
    fn null_handles_are_safe() {
        unsafe {
            assert_eq!(hz_game_texture_count(std::ptr::null()), 0);
            assert_eq!(hz_game_air_id(std::ptr::null()), 0);
            let mut out = [0f32; 7];
            assert_eq!(
                hz_game_raycast(
                    std::ptr::null(),
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    -1.0,
                    0.0,
                    8.0,
                    out.as_mut_ptr()
                ),
                0
            );
            assert_eq!(hz_game_save(std::ptr::null(), c"/tmp/x.hzw".as_ptr()), 1);
        }
    }
}
