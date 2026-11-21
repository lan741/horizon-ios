//! hz-game — the Rust simulation the iPad app renders with Metal.
//!
//! This is the part that "Minecraft Java → Rust" actually produces: a native
//! engine that consumes compiled `.hzp` descriptors instead of parsing JSON at
//! runtime, and that knows nothing about Metal, UIKit or Swift. The presentation
//! layer (Swift) pulls a vertex buffer per frame from [`Game::frame`].
//!
//! Deliberately small: palette-compressed chunks, a player capsule with AABB
//! collision, and a face-culled frame builder. Enough to prove the pipeline
//! end to end on device, and the seam where the rest of the game grows.

pub mod chunk;
pub mod frame;
pub mod game;
pub mod light;
pub mod mesh;
pub mod registry;
pub mod shader;
pub mod sound;
pub mod world_io;

pub use chunk::Chunk;
pub use frame::Frame;
pub use game::{Game, Player};
pub use light::{Bounds, HeightMap, LightField};
pub use registry::{BlockDef, Model, Registry};
pub use sound::{SoundCatalog, SoundQueue, SoundRequest};

/// 16³ sections; Minecraft's chunk column is 24 of these in 26.x, but the
/// engine indexes sections directly so the height does not matter here.
pub const SECTION: i32 = 16;
