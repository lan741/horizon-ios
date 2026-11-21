//! Texture animation descriptors (`.png.mcmeta`) → `AN1` bytes.
//!
//! The binary format and the frame schedule live in `hz_core::anim`; this module
//! only translates Mojang's JSON into it.

use hz_core::anim::{Animation, DEFAULT_FRAMETIME, MAX_FRAMES};
use hz_core::Json;

/// Parse one `.png.mcmeta` body. Returns `None` when the sidecar carries no
/// animation section (empty sidecars are legal in vanilla packs).
pub fn compile(texture_id: &str, meta: &str) -> Result<Option<Vec<u8>>, String> {
    let json = Json::parse(meta)?;
    let anim = match json.get("animation") {
        Some(a) => a,
        None => return Ok(None),
    };
    let frametime = anim
        .get("frametime")
        .and_then(|v| v.as_f64())
        .map(|v| v.clamp(1.0, 255.0) as u16)
        .unwrap_or(DEFAULT_FRAMETIME);
    let interpolate = anim
        .get("interpolate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut indices: Vec<u16> = Vec::new();
    if let Some(frames) = anim.get("frames").and_then(|f| f.as_arr()) {
        for frame in frames {
            match frame {
                Json::Num(n) => indices.push((*n as i64).clamp(0, u16::MAX as i64) as u16),
                Json::Obj(_) => {
                    let index = frame
                        .get("index")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0)
                        .max(0.0) as u16;
                    // `time` multiplies this frame's duration; repeat the index
                    // so a single uniform frametime still yields that schedule.
                    let repeat = frame
                        .get("time")
                        .and_then(|v| v.as_f64())
                        .map(|t| t.max(1.0).round() as u32)
                        .unwrap_or(1)
                        .min(MAX_FRAMES as u32);
                    for _ in 0..repeat {
                        indices.push(index);
                    }
                }
                _ => {}
            }
        }
    }
    if indices.is_empty() {
        // Vanilla implies a vertical strip of equal frames when `frames` is
        // absent, but the count needs the PNG height, which is not known here.
        // One frame is the honest answer: the texture shows up static.
        indices.push(0);
    }
    if indices.len() > MAX_FRAMES {
        return Err(format!(
            "animation {} has {} frames after expanding times",
            texture_id,
            indices.len()
        ));
    }
    Ok(Some(
        Animation {
            frametime,
            indices,
            interpolate,
        }
        .to_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_interpolate_flag() {
        let smooth = compile("x", r#"{"animation":{"interpolate":true,"frames":[0,1]}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(&smooth[..3], b"AN2");
        assert!(Animation::from_bytes(&smooth).unwrap().interpolate);

        let stepped = compile("x", r#"{"animation":{"frames":[0,1]}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(&stepped[..3], b"AN1");
        assert!(!Animation::from_bytes(&stepped).unwrap().interpolate);
    }

    #[test]
    fn parses_frame_list_and_time_override() {
        let meta = r#"{"animation":{"frametime":2,"frames":[0,1,{"index":2,"time":3},3]}}"#;
        let bytes = compile("minecraft:block/lava", meta).unwrap().unwrap();
        let anim = Animation::from_bytes(&bytes).unwrap();
        assert_eq!(anim.frametime, 2);
        assert_eq!(anim.indices, vec![0, 1, 2, 2, 2, 3]);
    }

    #[test]
    fn missing_animation_section_is_none() {
        assert!(compile("x", r#"{"texture":{"blur":true}}"#)
            .unwrap()
            .is_none());
    }

    #[test]
    fn empty_frames_degrades_to_static() {
        let bytes = compile("x", r#"{"animation":{}}"#).unwrap().unwrap();
        let anim = Animation::from_bytes(&bytes).unwrap();
        assert_eq!(anim.indices, vec![0]);
        assert_eq!(anim.frame_at(9999), 0);
    }

    #[test]
    fn rejects_non_json() {
        assert!(compile("x", "not json").is_err());
    }
}
