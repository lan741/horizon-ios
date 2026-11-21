//! hz-core — the dependency-free base layer of HorizonPad.
//!
//! Everything the iPad app needs to read Mojang's own files on device:
//! inflate/zip, JSON, NBT, hashes and the native pack container.
//!
//! No third-party crates: this crate must build offline for
//! `aarch64-apple-ios` in CI and must stay auditable for App Store review.

// `chunks_exact(64)` on a hash block and the CRC table loop are deliberately
// written the obvious way; these lints only add noise.
#![allow(clippy::needless_range_loop)]
#![allow(clippy::unusual_byte_groupings)]

pub mod anim;
pub mod inflate;
pub mod json;
pub mod nbt;
pub mod ogg;
pub mod pack;
pub mod sha;
pub mod util;
pub mod zipfile;

pub use json::Json;
pub use pack::{Pack, PackEntry};
pub use util::Buf;

/// Entry kinds inside a `.hzp` pack. Values are on disk: never renumber.
pub mod kind {
    pub const BLOCKSTATES: u8 = 1;
    pub const MODEL: u8 = 2;
    pub const LANG: u8 = 3;
    pub const TEXTURE: u8 = 4;
    pub const DATA: u8 = 5;
    pub const MIXIN: u8 = 6;
    pub const MANIFEST: u8 = 7;
    pub const SOUND: u8 = 8;
    pub const SHADER: u8 = 9;
    pub const ANIM: u8 = 10;
    /// Translated Metal source for a shader pack (generated at import time).
    pub const SHADER_MSL: u8 = 11;
}

pub fn kind_name(k: u8) -> &'static str {
    match k {
        kind::BLOCKSTATES => "blockstates",
        kind::MODEL => "model",
        kind::LANG => "lang",
        kind::TEXTURE => "texture",
        kind::DATA => "data",
        kind::MIXIN => "mixin",
        kind::MANIFEST => "manifest",
        kind::SOUND => "sound",
        kind::SHADER => "shader",
        kind::ANIM => "animation",
        kind::SHADER_MSL => "shader_msl",
        _ => "unknown",
    }
}
