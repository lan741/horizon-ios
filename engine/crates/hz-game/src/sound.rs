//! Sound catalog and the playback request queue.
//!
//! The engine does not decode or mix audio: it decides *what* should play and
//! *how loud*, and hands that to the platform. That keeps `hz-game` free of
//! Apple frameworks (so it stays testable on Windows) and keeps the one piece
//! that is platform-specific — decoding Ogg Vorbis — in one place.
//!
//! ```text
//! app tick ──► Game::play_sound("block.stone.break", x, y, z)
//!                 │ variant chosen round-robin, volume attenuated by distance
//!                 ▼
//!            request queue ──► hz_game_next_sound() ──► AVAudioPlayerNode
//! ```

use std::collections::HashMap;

use hz_core::ogg::{self, OggInfo};
use hz_core::util::Reader;

/// Distance beyond which a sound is inaudible, matching vanilla's 16-block
/// linear falloff for most events.
pub const MAX_AUDIBLE_DISTANCE: f32 = 16.0;

/// How many pending requests to keep. A burst of events should not grow without
/// bound if the platform stops draining.
pub const MAX_QUEUED: usize = 64;

pub struct SoundVariant {
    /// Pack entry id under `sounds/`, e.g. `minecraft:dig/stone1`.
    pub resource: String,
    pub weight: u8,
    pub streamed: bool,
    pub volume: f32,
    pub pitch: f32,
    /// Index into `SoundCatalog::assets`, or `u16::MAX` when the payload is
    /// missing from the pack.
    pub asset: u16,
}

pub struct SoundEvent {
    pub name: String,
    pub variants: Vec<SoundVariant>,
}

pub struct SoundAsset {
    pub id: String,
    pub bytes: Vec<u8>,
    /// Container facts, parsed at load time (channels, rate, duration).
    pub info: OggInfo,
}

/// One thing to play, resolved to an asset index.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundRequest {
    pub asset: u16,
    pub volume: f32,
    pub pitch: f32,
}

#[derive(Default)]
pub struct SoundCatalog {
    pub events: Vec<SoundEvent>,
    pub assets: Vec<SoundAsset>,
    by_name: HashMap<String, u16>,
    /// Round-robin cursor per event, so repeated plays do not always pick the
    /// same variant. Deterministic, which is what the tests want.
    cursor: HashMap<u16, u32>,
}

impl SoundCatalog {
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn asset_count(&self) -> usize {
        self.assets.len()
    }

    pub fn event_id(&self, name: &str) -> Option<u16> {
        self.by_name.get(name).copied()
    }

    pub fn asset(&self, index: u16) -> Option<&SoundAsset> {
        self.assets.get(index as usize)
    }

    /// Pick the next variant for `event` and return the request it produces.
    /// `gain` scales the variant's own volume (e.g. distance attenuation).
    pub fn request(&mut self, event: u16, gain: f32) -> Option<SoundRequest> {
        let cursor = {
            let entry = self.cursor.entry(event).or_insert(0);
            let value = *entry;
            *entry = entry.wrapping_add(1);
            value
        };
        let sound_event = self.events.get(event as usize)?;
        if sound_event.variants.is_empty() {
            return None;
        }
        // Weighted pick without an RNG: walk the weighted list by a rotating
        // cursor so a long session still hears every variant evenly.
        let total: u32 = sound_event
            .variants
            .iter()
            .map(|v| v.weight.max(1) as u32)
            .sum();
        let mut pick = cursor % total.max(1);
        let mut chosen = &sound_event.variants[0];
        for variant in &sound_event.variants {
            let weight = variant.weight.max(1) as u32;
            if pick < weight {
                chosen = variant;
                break;
            }
            pick -= weight;
        }
        if chosen.asset == u16::MAX {
            return None; // no payload in the pack
        }
        Some(SoundRequest {
            asset: chosen.asset,
            volume: (chosen.volume * gain).clamp(0.0, 4.0),
            pitch: chosen.pitch,
        })
    }

    /// Linear falloff over [`MAX_AUDIBLE_DISTANCE`], returning 0 when out of
    /// range. Vanilla uses the same shape for most events.
    pub fn attenuation(distance: f32) -> f32 {
        if distance >= MAX_AUDIBLE_DISTANCE {
            0.0
        } else {
            1.0 - (distance / MAX_AUDIBLE_DISTANCE)
        }
    }

    /// Build from pack entries: `soundcatalog/*` carry `SND1`, `sounds/*` carry
    /// Ogg payloads. Variants whose payload is missing keep `asset = u16::MAX`
    /// and are skipped at play time.
    pub fn build(entries: &[(u8, String, Vec<u8>)]) -> Result<SoundCatalog, String> {
        let mut catalog = SoundCatalog::default();

        // Payloads first: the catalog refers to them by id.
        for (kind, name, data) in entries {
            if *kind != hz_core::kind::SOUND {
                continue;
            }
            let id = match name.strip_prefix("sounds/") {
                Some(id) => id.to_string(),
                None => continue,
            };
            let info = ogg::parse(data).unwrap_or(OggInfo {
                vorbis: None,
                packets: 0,
                last_granule: 0,
                checksums_ok: false,
                pages: 0,
                bytes: data.len(),
            });
            catalog.assets.push(SoundAsset {
                id,
                bytes: data.clone(),
                info,
            });
        }
        let mut asset_index: HashMap<&str, u16> = HashMap::new();
        for (i, asset) in catalog.assets.iter().enumerate() {
            asset_index.insert(asset.id.as_str(), i as u16);
        }

        for (kind, name, data) in entries {
            if *kind != hz_core::kind::SOUND || name.starts_with("sounds/") {
                continue;
            }
            let mut events = decode_catalog(data)?;
            for event in events.iter_mut() {
                for variant in event.variants.iter_mut() {
                    variant.asset = asset_index
                        .get(variant.resource.as_str())
                        .copied()
                        .unwrap_or(u16::MAX);
                }
                let id = catalog.events.len() as u16;
                catalog.by_name.insert(event.name.clone(), id);
                catalog.events.push(SoundEvent {
                    name: event.name.clone(),
                    variants: std::mem::take(&mut event.variants),
                });
            }
        }
        Ok(catalog)
    }
}

/// Decode a `SND1` blob into events with unresolved assets.
fn decode_catalog(bytes: &[u8]) -> Result<Vec<SoundEvent>, String> {
    let mut r = Reader::new(bytes);
    if r.take(4)? != b"SND1" {
        return Err("sound catalog: bad magic".to_string());
    }
    let count = r.u16()? as usize;
    let mut events = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let name = r.str16()?;
        let variant_count = r.u16()? as usize;
        let mut variants = Vec::with_capacity(variant_count.min(64));
        for _ in 0..variant_count {
            let resource = r.str16()?;
            let weight = r.u8()?;
            let flags = r.u8()?;
            let volume = r.f32()?;
            let pitch = r.f32()?;
            variants.push(SoundVariant {
                resource,
                weight,
                streamed: flags & 1 != 0,
                volume,
                pitch,
                asset: u16::MAX,
            });
        }
        events.push(SoundEvent { name, variants });
    }
    Ok(events)
}

/// Pending playback requests; bounded so a burst cannot grow without limit.
#[derive(Default)]
pub struct SoundQueue {
    items: std::collections::VecDeque<SoundRequest>,
}

impl SoundQueue {
    pub fn push(&mut self, request: SoundRequest) {
        if self.items.len() >= MAX_QUEUED {
            self.items.pop_front();
        }
        self.items.push_back(request);
    }

    pub fn pop(&mut self) -> Option<SoundRequest> {
        self.items.pop_front()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
pub mod testutil {
    use hz_core::ogg::testutil::{page, vorbis_id};

    /// Smallest valid Ogg we can build: one ident packet plus one audio page.
    pub fn tiny_ogg(sample_rate: u32) -> Vec<u8> {
        let id = vorbis_id(1, sample_rate);
        let mut bytes = page(1, 0, 0x02, 0, &[&id]);
        bytes.extend(page(1, 1, 0x04, sample_rate as u64, &[b"\x00audio"]));
        bytes
    }

    pub fn catalog_bytes(events: &[(&str, &[(&str, u8)])]) -> Vec<u8> {
        let mut b = hz_core::Buf::new();
        b.bytes(b"SND1");
        b.u16(events.len() as u16);
        for (name, variants) in events {
            b.str16(name);
            b.u16(variants.len() as u16);
            for (resource, weight) in *variants {
                b.str16(resource);
                b.u8(*weight);
                b.u8(0);
                b.f32(1.0);
                b.f32(1.0);
            }
        }
        b.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{catalog_bytes, tiny_ogg};
    use super::*;
    use hz_core::kind;

    fn catalog() -> SoundCatalog {
        let entries = vec![
            (
                kind::SOUND,
                "soundcatalog/minecraft".to_string(),
                catalog_bytes(&[
                    (
                        "block.stone.break",
                        &[("minecraft:dig/stone1", 1), ("minecraft:dig/stone2", 1)][..],
                    ),
                    (
                        "entity.player.levelup",
                        &[("minecraft:random/levelup", 1)][..],
                    ),
                ]),
            ),
            (
                kind::SOUND,
                "sounds/minecraft:dig/stone1".to_string(),
                tiny_ogg(44100),
            ),
            (
                kind::SOUND,
                "sounds/minecraft:dig/stone2".to_string(),
                tiny_ogg(22050),
            ),
            // levelup payload deliberately absent: its variant must be skipped
        ];
        SoundCatalog::build(&entries).unwrap()
    }

    #[test]
    fn builds_events_and_resolves_assets() {
        let c = catalog();
        assert_eq!(c.event_count(), 2);
        assert_eq!(c.asset_count(), 2);
        let stone = &c.events[c.event_id("block.stone.break").unwrap() as usize];
        assert_eq!(stone.variants.len(), 2);
        assert_ne!(stone.variants[0].asset, u16::MAX);
        let asset = c.asset(stone.variants[0].asset).unwrap();
        assert_eq!(asset.info.vorbis.as_ref().unwrap().sample_rate, 44100);
        assert!(asset.info.checksums_ok);
    }

    #[test]
    fn requests_rotate_through_variants() {
        let mut c = catalog();
        let id = c.event_id("block.stone.break").unwrap();
        let first = c.request(id, 1.0).unwrap();
        let second = c.request(id, 1.0).unwrap();
        assert_ne!(first.asset, second.asset, "variants rotate");
        let third = c.request(id, 1.0).unwrap();
        assert_eq!(third.asset, first.asset, "and wrap around");
    }

    #[test]
    fn missing_payload_yields_no_request() {
        let mut c = catalog();
        let id = c.event_id("entity.player.levelup").unwrap();
        assert!(c.request(id, 1.0).is_none());
    }

    #[test]
    fn attenuation_is_linear_and_capped() {
        assert_eq!(SoundCatalog::attenuation(0.0), 1.0);
        assert!((SoundCatalog::attenuation(8.0) - 0.5).abs() < 1e-6);
        assert_eq!(SoundCatalog::attenuation(16.0), 0.0);
        assert_eq!(SoundCatalog::attenuation(40.0), 0.0);
    }

    #[test]
    fn gain_scales_the_request_volume() {
        let mut c = catalog();
        let id = c.event_id("block.stone.break").unwrap();
        let quiet = c.request(id, 0.25).unwrap();
        assert!((quiet.volume - 0.25).abs() < 1e-6);
    }

    #[test]
    fn unknown_events_are_simply_absent() {
        let c = catalog();
        assert!(c.event_id("no.such.event").is_none());
    }

    #[test]
    fn queue_is_bounded_and_fifo() {
        let mut q = SoundQueue::default();
        for i in 0..(MAX_QUEUED + 10) {
            q.push(SoundRequest {
                asset: i as u16,
                volume: 1.0,
                pitch: 1.0,
            });
        }
        assert_eq!(q.len(), MAX_QUEUED, "oldest are dropped, not the newest");
        let first = q.pop().unwrap();
        assert_eq!(first.asset, 10);
        q.clear();
        assert!(q.is_empty());
    }

    #[test]
    fn rejects_a_corrupt_catalog() {
        let entries = vec![(
            kind::SOUND,
            "soundcatalog/minecraft".to_string(),
            b"XXXX".to_vec(),
        )];
        assert!(SoundCatalog::build(&entries).is_err());
    }
}
