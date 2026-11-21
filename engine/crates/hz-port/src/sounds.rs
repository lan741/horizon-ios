//! `sounds.json` → `SND1`, the compiled sound catalog.
//!
//! Minecraft's `assets/<ns>/sounds.json` maps an event name to a list of
//! candidate sounds, each with a volume, a pitch and a weight. The sound files
//! themselves live beside it as `sounds/<path>.ogg`.
//!
//! ```text
//! "SND1" | u16 event_count
//! per event:
//!   str16 name
//!   u16 variant_count
//!   per variant:
//!     str16 resource_id      (e.g. "minecraft:block.stone.break")
//!     u8   weight
//!     u8   flags             bit0 streamed, bit1 subtitle present
//!     f32  volume
//!     f32  pitch
//! ```
//!
//! The resource id is the pack entry name under `sounds/`, so the runtime never
//! parses JSON and never guesses file names.
//!
//! Deliberately not ported: subtitles and `type` beyond the streamed flag. The
//! Ogg payloads are kept as-is — decoding Vorbis is the platform's job, and on
//! iOS it is not available at all (see docs/SOUND.md).

use hz_core::Json;

pub const FLAG_STREAMED: u8 = 1;
pub const FLAG_SUBTITLE: u8 = 2;

pub struct Variant {
    pub resource: String,
    pub weight: u8,
    pub flags: u8,
    pub volume: f32,
    pub pitch: f32,
}

pub struct Event {
    pub name: String,
    pub variants: Vec<Variant>,
}

/// `"block.stone.break"` in namespace `minecraft` → `minecraft:block.stone.break`.
fn qualified(namespace: &str, name: &str) -> String {
    if name.contains(':') {
        name.to_string()
    } else {
        format!("{}:{}", namespace, name)
    }
}

/// Parse one `sounds.json`. `namespace` comes from the asset path
/// (`assets/<ns>/sounds.json`).
pub fn compile(namespace: &str, text: &str) -> Result<Vec<Event>, String> {
    let json = Json::parse(text)?;
    let obj = json
        .as_obj()
        .ok_or_else(|| "sounds.json: top level is not an object".to_string())?;

    let mut events = Vec::with_capacity(obj.len());
    for (event_name, value) in obj {
        let mut variants = Vec::new();
        if let Some(sounds) = value.get("sounds").and_then(|s| s.as_arr()) {
            for entry in sounds {
                match entry {
                    Json::Str(s) => variants.push(Variant {
                        resource: qualified(namespace, s),
                        weight: 1,
                        flags: 0,
                        volume: 1.0,
                        pitch: 1.0,
                    }),
                    Json::Obj(_) => {
                        let name = match entry.get("name").and_then(|v| v.as_str()) {
                            Some(n) => n,
                            None => continue,
                        };
                        let stream = entry
                            .get("stream")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let subtitle = entry.get("subtitle").and_then(|v| v.as_str());
                        variants.push(Variant {
                            resource: qualified(namespace, name),
                            weight: entry
                                .get("weight")
                                .and_then(|v| v.as_f64())
                                .map(|w| w.clamp(1.0, 255.0) as u8)
                                .unwrap_or(1),
                            flags: (if stream { FLAG_STREAMED } else { 0 })
                                | (if subtitle.is_some() { FLAG_SUBTITLE } else { 0 }),
                            volume: entry
                                .get("volume")
                                .and_then(|v| v.as_f64())
                                .map(|v| v.clamp(0.0, 4.0) as f32)
                                .unwrap_or(1.0),
                            pitch: entry
                                .get("pitch")
                                .and_then(|v| v.as_f64())
                                .map(|v| v.clamp(0.01, 4.0) as f32)
                                .unwrap_or(1.0),
                        });
                    }
                    _ => {}
                }
            }
        }
        if variants.is_empty() {
            continue; // an event with no playable sound is not a sound
        }
        events.push(Event {
            name: event_name.clone(),
            variants,
        });
    }
    Ok(events)
}

/// Serialize a compiled catalog.
pub fn encode(events: &[Event]) -> Vec<u8> {
    use hz_core::Buf;
    let mut b = Buf::with_capacity(1024);
    b.bytes(b"SND1");
    b.u16(events.len().min(u16::MAX as usize) as u16);
    for event in events.iter().take(u16::MAX as usize) {
        b.str16(&event.name);
        b.u16(event.variants.len().min(u16::MAX as usize) as u16);
        for v in event.variants.iter().take(u16::MAX as usize) {
            b.str16(&v.resource);
            b.u8(v.weight);
            b.u8(v.flags);
            b.f32(v.volume);
            b.f32(v.pitch);
        }
    }
    b.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "block.stone.break": {
            "category": "block",
            "sounds": [
                { "name": "dig/stone1", "volume": 0.9, "pitch": 1.1, "weight": 3 },
                "dig/stone2",
                { "name": "dig/stone_long", "stream": true, "subtitle": "subtitles.block.generic.break" }
            ]
        },
        "entity.player.levelup": { "sounds": [ { "name": "random/levelup" } ] },
        "empty.event": { "sounds": [] }
    }"#;

    #[test]
    fn compiles_events_and_variants() {
        let events = compile("minecraft", SAMPLE).unwrap();
        // the empty event is dropped rather than emitted unplayable
        assert_eq!(events.len(), 2);
        let stone = &events[0];
        assert_eq!(stone.name, "block.stone.break");
        assert_eq!(stone.variants.len(), 3);
        assert_eq!(stone.variants[0].resource, "minecraft:dig/stone1");
        assert_eq!(stone.variants[0].weight, 3);
        assert!((stone.variants[0].volume - 0.9).abs() < 1e-6);
        assert!((stone.variants[0].pitch - 1.1).abs() < 1e-4);
        assert_eq!(stone.variants[1].weight, 1, "a bare string defaults to 1");
        assert_eq!(stone.variants[2].flags, FLAG_STREAMED | FLAG_SUBTITLE);
    }

    #[test]
    fn namespaces_and_qualified_names() {
        let events = compile(
            "my_pack",
            r#"{"x": {"sounds": ["a/b"]}, "y": {"sounds": ["other:a/b"]}}"#,
        )
        .unwrap();
        assert_eq!(events[0].variants[0].resource, "my_pack:a/b");
        assert_eq!(events[1].variants[0].resource, "other:a/b");
    }

    #[test]
    fn survives_malformed_entries() {
        let events = compile("minecraft", r#"{"a": {"sounds": [{}, {"name": 5}, "ok"]}}"#).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].variants.len(), 1, "only the valid entry survives");
        assert_eq!(events[0].variants[0].resource, "minecraft:ok");
    }

    #[test]
    fn rejects_non_object_input() {
        assert!(compile("minecraft", "[1,2,3]").is_err());
        assert!(compile("minecraft", "not json").is_err());
    }

    #[test]
    fn roundtrips_through_bytes() {
        let events = compile("minecraft", SAMPLE).unwrap();
        let bytes = encode(&events);
        assert_eq!(&bytes[..4], b"SND1");
        // The engine decoder is exercised in hz-game; here we only check that
        // the header and counts are sane.
        let count = u16::from_le_bytes([bytes[4], bytes[5]]);
        assert_eq!(count as usize, events.len());
    }
}
