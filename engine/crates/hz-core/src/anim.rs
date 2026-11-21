//! Animated texture descriptors, shared by the porter and the runtime.
//!
//! Minecraft stores an animated texture as one tall PNG plus a sidecar that says
//! how to slice it. Rather than split the image (which would mean decoding and
//! re-encoding PNG on device), the pack carries this descriptor and the mesher
//! turns frame `f` of `n` into a V offset: `v' = (v + f) / n`.
//!
//! ```text
//! "AN1" | u16 frametime | u16 frame_count | u16 indices[frame_count]
//! "AN2" | u16 frametime | u16 frame_count | u16 flags | u16 indices[…]
//! ```
//! `AN2` adds `flags` bit0 = interpolate. `AN1` still loads (interpolation off),
//! so packs made before interpolation existed keep working.
//!
//! Interpolation does not blur the image: the vertex carries the V offset of the
//! *next* frame plus a blend factor, and the shader crossfades two nearest
//! samples. That keeps pixel art crisp and still gives a smooth transition.

use crate::util::{Buf, Reader};

/// Default frame time in ticks: vanilla shows 20 frames per second.
pub const DEFAULT_FRAMETIME: u16 = 1;
/// Upper bound on frames after expanding per-frame `time` overrides.
pub const MAX_FRAMES: usize = 4096;
/// `AN2` flags bit0.
pub const FLAG_INTERPOLATE: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Animation {
    pub frametime: u16,
    /// Frame order. Per-frame `time` overrides are folded in by repeating an
    /// index, so the runtime only ever needs one uniform frametime.
    pub indices: Vec<u16>,
    /// Crossfade between consecutive frames instead of stepping.
    pub interpolate: bool,
}

/// What the mesher needs for one quad: the frame to show and, when
/// interpolating, the frame to fade into and how far into that fade we are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FramePair {
    /// Frame index inside the strip.
    pub current: u16,
    /// Next frame index; equal to `current` when not interpolating.
    pub next: u16,
    /// 0..1 across the current frame's duration.
    pub blend: f32,
}

impl Animation {
    pub fn frame_count(&self) -> usize {
        self.indices.len()
    }

    /// Frame index inside the strip to show at `tick`.
    pub fn frame_at(&self, tick: u64) -> u16 {
        if self.indices.is_empty() {
            return 0;
        }
        let step = tick / self.frametime.max(1) as u64;
        let slot = (step as usize) % self.indices.len();
        self.indices[slot]
    }

    /// Current frame plus the fade target and progress.
    ///
    /// `blend` is only meaningful when `interpolate` is set; otherwise it is 0
    /// and `next == current`, which lets the shader take a single sample path.
    pub fn frame_pair(&self, tick: u64) -> FramePair {
        let current = self.frame_at(tick);
        if !self.interpolate || self.indices.len() < 2 {
            return FramePair {
                current,
                next: current,
                blend: 0.0,
            };
        }
        let frametime = self.frametime.max(1) as u64;
        let step = tick / frametime;
        let into_frame = (tick % frametime) as f32 / frametime as f32;
        let next_slot = ((step as usize) + 1) % self.indices.len();
        FramePair {
            current,
            next: self.indices[next_slot],
            blend: into_frame,
        }
    }

    /// V scale for one frame: `v' = v / frames + frame / frames`.
    pub fn v_scale(&self) -> f32 {
        1.0 / self.frame_count().max(1) as f32
    }

    /// V distance from `current` to `next`, for the shader's second sample.
    pub fn v_delta(&self, pair: FramePair) -> f32 {
        (pair.next as i32 - pair.current as i32) as f32 * self.v_scale()
    }

    /// Legacy helper: `(v_offset, v_scale)` for the discrete frame at `tick`.
    pub fn v_transform(&self, tick: u64) -> (f32, f32) {
        let frame = self.frame_at(tick);
        (frame as f32 * self.v_scale(), self.v_scale())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Buf::new();
        if self.interpolate {
            b.bytes(b"AN2");
            b.u16(self.frametime);
            b.u16(self.indices.len() as u16);
            b.u16(FLAG_INTERPOLATE);
        } else {
            // Keep AN1 for the common case so older engines read newer packs.
            b.bytes(b"AN1");
            b.u16(self.frametime);
            b.u16(self.indices.len() as u16);
        }
        for i in &self.indices {
            b.u16(*i);
        }
        b.finish()
    }

    pub fn from_bytes(d: &[u8]) -> Result<Animation, String> {
        let mut r = Reader::new(d);
        let magic = r.take(3)?;
        let mut interpolate = false;
        let (frametime, n) = match magic {
            b"AN1" => (r.u16()?.max(1), r.u16()? as usize),
            b"AN2" => {
                let frametime = r.u16()?.max(1);
                let n = r.u16()? as usize;
                let flags = r.u16()?;
                interpolate = flags & FLAG_INTERPOLATE != 0;
                (frametime, n)
            }
            _ => return Err("animation: bad magic".to_string()),
        };
        if n > MAX_FRAMES {
            return Err(format!("animation: {} frames is implausible", n));
        }
        let mut indices = Vec::with_capacity(n);
        for _ in 0..n {
            indices.push(r.u16()?);
        }
        Ok(Animation {
            frametime,
            indices,
            interpolate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(n: u16) -> Animation {
        Animation {
            frametime: 2,
            indices: (0..n).collect(),
            interpolate: false,
        }
    }

    #[test]
    fn frame_schedule_wraps() {
        let anim = strip(4);
        assert_eq!(anim.frame_at(0), 0);
        assert_eq!(anim.frame_at(1), 0, "frametime 2 means two ticks per frame");
        assert_eq!(anim.frame_at(2), 1);
        assert_eq!(anim.frame_at(6), 3);
        assert_eq!(anim.frame_at(8), 0, "wraps around");
    }

    #[test]
    fn v_transform_covers_the_strip() {
        let anim = strip(4);
        assert_eq!(anim.v_transform(0), (0.0, 0.25));
        assert_eq!(anim.v_transform(4), (0.5, 0.25));
    }

    #[test]
    fn interpolation_reports_the_next_frame_and_progress() {
        let mut anim = strip(4);
        anim.interpolate = true;
        // tick 0: frame 0, nothing blended yet
        let p = anim.frame_pair(0);
        assert_eq!((p.current, p.next), (0, 1));
        assert_eq!(p.blend, 0.0);
        // tick 1 of a 2-tick frame: halfway to frame 1
        let p = anim.frame_pair(1);
        assert_eq!((p.current, p.next), (0, 1));
        assert!((p.blend - 0.5).abs() < 1e-6);
        // tick 2: now on frame 1, fade toward 2
        let p = anim.frame_pair(2);
        assert_eq!((p.current, p.next), (1, 2));
        assert_eq!(p.blend, 0.0);
        // last frame fades back around to frame 0
        let p = anim.frame_pair(6);
        assert_eq!((p.current, p.next), (3, 0));
        assert!((anim.v_delta(p) - (-0.75)).abs() < 1e-6);
    }

    #[test]
    fn no_interpolation_keeps_blend_zero_and_next_equal() {
        let anim = strip(4);
        let p = anim.frame_pair(1);
        assert_eq!((p.current, p.next), (0, 0));
        assert_eq!(p.blend, 0.0);
        assert_eq!(anim.v_delta(p), 0.0);
    }

    #[test]
    fn single_frame_never_interpolates() {
        let mut anim = Animation {
            frametime: 1,
            indices: vec![0],
            interpolate: true,
        };
        anim.interpolate = true;
        let p = anim.frame_pair(7);
        assert_eq!((p.current, p.next), (0, 0));
        assert_eq!(p.blend, 0.0);
    }

    #[test]
    fn roundtrips_both_versions() {
        let plain = strip(3);
        assert_eq!(Animation::from_bytes(&plain.to_bytes()).unwrap(), plain);

        let mut smooth = strip(3);
        smooth.interpolate = true;
        let bytes = smooth.to_bytes();
        assert_eq!(&bytes[..3], b"AN2");
        assert_eq!(Animation::from_bytes(&bytes).unwrap(), smooth);

        // An AN1 stream still parses, with interpolation off.
        let legacy = {
            let mut b = Buf::new();
            b.bytes(b"AN1");
            b.u16(1);
            b.u16(2);
            b.u16(0);
            b.u16(1);
            b.finish()
        };
        let parsed = Animation::from_bytes(&legacy).unwrap();
        assert_eq!(parsed.indices, vec![0, 1]);
        assert!(!parsed.interpolate);
    }

    #[test]
    fn rejects_bad_magic_and_giant_lists() {
        assert!(Animation::from_bytes(b"NOPE").is_err());
        let mut b = Buf::new();
        b.bytes(b"AN2");
        b.u16(1);
        b.u16(MAX_FRAMES as u16 + 1);
        b.u16(0);
        assert!(Animation::from_bytes(&b.finish()).is_err());
    }
}
