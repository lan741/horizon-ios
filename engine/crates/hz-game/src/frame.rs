//! Frame assembly: what the Swift/Metal side consumes.
//!
//! Interleaved vertices, no index buffer (two triangles per quad, emitted in
//! order). Layout per vertex, 16 × f32 = 64 bytes so it stays 16-byte aligned
//! and matches a Metal vertex stride without padding surprises:
//!
//! `px py pz | u v | nx ny nz | r g b a | texture | v_delta | blend | 0`
//!
//! `texture` is a float holding an index into the renderer's `texture2d_array`;
//! `u16::MAX` (no texture) arrives as 65535.0 and the shader treats anything
//! beyond the array as "use the vertex colour".
//!
//! `v_delta` + `blend` drive animated-texture interpolation: the shader takes a
//! second sample at `uv + (0, v_delta)` and mixes by `blend`. Non-animated quads
//! carry 0 / 0, so the second sample lands on the same texel and costs nothing
//! beyond one extra fetch.

pub const FLOATS_PER_VERTEX: usize = 16;
/// Byte distance between vertices; the Metal descriptor's `stride`.
pub const VERTEX_STRIDE: usize = FLOATS_PER_VERTEX * 4;
pub const VERTS_PER_QUAD: usize = 6;

#[derive(Default)]
pub struct Frame {
    pub verts: Vec<f32>,
    pub quads: u32,
}

impl Frame {
    pub fn new() -> Frame {
        Frame::default()
    }

    pub fn clear(&mut self) {
        self.verts.clear();
        self.quads = 0;
    }

    pub fn vertex_count(&self) -> u32 {
        (self.verts.len() / FLOATS_PER_VERTEX) as u32
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        p: [f32; 3],
        uv: [f32; 2],
        n: [f32; 3],
        c: [f32; 4],
        texture: f32,
        v_delta: f32,
        blend: f32,
    ) {
        self.verts.extend_from_slice(&[
            p[0], p[1], p[2], uv[0], uv[1], n[0], n[1], n[2], c[0], c[1], c[2], c[3], texture,
            v_delta, blend, 0.0,
        ]);
    }

    /// Push one quad. `corners` must be counter-clockwise seen from outside.
    #[allow(clippy::too_many_arguments)]
    pub fn push_quad(
        &mut self,
        corners: &[[f32; 3]; 4],
        uv: &[[f32; 2]; 4],
        normal: [f32; 3],
        color: [f32; 4],
        texture: u16,
    ) {
        let t = if texture == u16::MAX {
            f32::from(u16::MAX)
        } else {
            f32::from(texture)
        };
        for i in [0usize, 1, 2, 2, 3, 0] {
            self.push(corners[i], uv[i], normal, color, t, 0.0, 0.0);
        }
        self.quads += 1;
    }

    /// Push one quad whose texture is interpolating between two frames.
    #[allow(clippy::too_many_arguments)]
    pub fn push_quad_animated(
        &mut self,
        corners: &[[f32; 3]; 4],
        uv: &[[f32; 2]; 4],
        normal: [f32; 3],
        color: [f32; 4],
        texture: u16,
        v_delta: f32,
        blend: f32,
    ) {
        let t = if texture == u16::MAX {
            f32::from(u16::MAX)
        } else {
            f32::from(texture)
        };
        for i in [0usize, 1, 2, 2, 3, 0] {
            self.push(corners[i], uv[i], normal, color, t, v_delta, blend);
        }
        self.quads += 1;
    }

    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: f32 has no padding and the slice is contiguous; length is a
        // multiple of 4 by construction. Read-only view for the FFI copy.
        unsafe {
            std::slice::from_raw_parts(
                self.verts.as_ptr() as *const u8,
                self.verts.len() * std::mem::size_of::<f32>(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_stable() {
        let mut f = Frame::new();
        let c = [[0.0, 0.0, 0.0]; 4];
        let uv = [[0.0, 0.0]; 4];
        f.push_quad(&c, &uv, [0.0, 1.0, 0.0], [1.0, 1.0, 1.0, 1.0], 3);
        assert_eq!(f.quads, 1);
        assert_eq!(f.vertex_count(), 6);
        assert_eq!(f.verts.len(), 6 * FLOATS_PER_VERTEX);
        assert_eq!(f.as_bytes().len(), 6 * VERTEX_STRIDE);
        // texture slot lands in the 13th float of each vertex
        for v in 0..6 {
            assert_eq!(f.verts[v * FLOATS_PER_VERTEX + 12], 3.0);
        }
    }
}
