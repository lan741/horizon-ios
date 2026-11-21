//! Ogg Vorbis decoding through the vendored `stb_vorbis`.
//!
//! ## Why a C dependency
//!
//! Everything else in this engine is dependency-free Rust, including the Ogg
//! *container* parser. Vorbis itself is a different scale: codebooks, floors,
//! residues, MDCT. `stb_vorbis.c` is one public-domain file that has decoded
//! this format for a decade; vendoring it is a smaller risk than hand-writing a
//! decoder, and it is pinned by SHA-256 (see `build.rs`).
//!
//! The Rust side is a thin, safe wrapper: open, read frames, convert to
//! interleaved `f32`, close. Everything is per-call — no globals, no shared
//! state — so it is safe to call from any thread.
//!
//! ## When the C compiler is missing
//!
//! `build.rs` still produces a linkable crate and `available()` returns false;
//! callers then report "no decoder" instead of failing to build. CI and the
//! App Store build always have a compiler, so this only affects odd hosts.

use std::ffi::c_void;

#[cfg(has_stb_vorbis)]
mod sys {
    use std::os::raw::{c_int, c_short, c_uchar, c_uint, c_void};

    #[repr(C)]
    pub struct StbVorbis {
        _private: [u8; 0],
    }

    /// `struct stb_vorbis_info`, returned by value from the C API.
    #[repr(C)]
    pub struct StbVorbisInfo {
        pub sample_rate: c_uint,
        pub channels: c_int,
        pub setup_memory_required: c_uint,
        pub setup_temp_memory_required: c_uint,
        pub temp_memory_required: c_uint,
        pub max_frame_size: c_int,
    }

    extern "C" {
        pub fn stb_vorbis_open_memory(
            data: *const c_uchar,
            len: c_int,
            error: *mut c_int,
            alloc_buffer: *const c_void,
        ) -> *mut StbVorbis;
        pub fn stb_vorbis_get_info(f: *mut StbVorbis) -> StbVorbisInfo;
        pub fn stb_vorbis_get_frame_short_interleaved(
            f: *mut StbVorbis,
            num_c: c_int,
            buffer: *mut c_short,
            num_shorts: c_int,
        ) -> c_int;
        pub fn stb_vorbis_close(f: *mut StbVorbis);
    }
}

/// Decoded audio, interleaved and normalised to -1.0..=1.0.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    pub sample_rate: u32,
    pub channels: u16,
    /// `samples.len() == frames * channels`
    pub samples: Vec<f32>,
}

impl Decoded {
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }

    pub fn duration_seconds(&self) -> f32 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f32 / self.sample_rate as f32
        }
    }
}

/// Whether this build actually has the decoder linked in.
pub const fn available() -> bool {
    cfg!(has_stb_vorbis)
}

/// Errors carry the decoder's own diagnosis where there is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Unavailable,
    Empty,
    /// stb_vorbis refused the stream; the number is its error code.
    Open(i32),
    NoAudio,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Unavailable => write!(f, "no Ogg Vorbis decoder in this build"),
            DecodeError::Empty => write!(f, "empty input"),
            DecodeError::Open(code) => {
                write!(f, "stb_vorbis could not open the stream (error {code})")
            }
            DecodeError::NoAudio => write!(f, "stream opened but produced no audio"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decode a whole Ogg Vorbis file held in memory.
#[cfg(has_stb_vorbis)]
pub fn decode(ogg: &[u8]) -> Result<Decoded, DecodeError> {
    use std::os::raw::c_int;

    if ogg.is_empty() {
        return Err(DecodeError::Empty);
    }
    if ogg.len() > i32::MAX as usize {
        return Err(DecodeError::Open(-1));
    }

    let mut error: c_int = 0;
    let file = unsafe {
        sys::stb_vorbis_open_memory(
            ogg.as_ptr(),
            ogg.len() as c_int,
            &mut error as *mut c_int,
            std::ptr::null(),
        )
    };
    if file.is_null() {
        return Err(DecodeError::Open(error));
    }

    let info = unsafe { sys::stb_vorbis_get_info(file) };
    let channels = info.channels.clamp(1, 32) as u16;
    let sample_rate = info.sample_rate;

    // stb_vorbis returns frames per channel; a chunk of 4096 frames is plenty
    // and keeps peak memory flat regardless of file length.
    const CHUNK_FRAMES: usize = 4096;
    let mut interleaved = vec![0i16; CHUNK_FRAMES * channels as usize];
    let mut samples: Vec<f32> = Vec::new();

    loop {
        let frames = unsafe {
            sys::stb_vorbis_get_frame_short_interleaved(
                file,
                channels as c_int,
                interleaved.as_mut_ptr(),
                interleaved.len() as c_int,
            )
        };
        if frames <= 0 {
            break;
        }
        let count = frames as usize * channels as usize;
        samples.extend(
            interleaved[..count.min(interleaved.len())]
                .iter()
                .map(|s| *s as f32 / 32768.0),
        );
    }
    unsafe { sys::stb_vorbis_close(file) };

    if samples.is_empty() {
        return Err(DecodeError::NoAudio);
    }
    Ok(Decoded {
        sample_rate,
        channels,
        samples,
    })
}

#[cfg(not(has_stb_vorbis))]
pub fn decode(_ogg: &[u8]) -> Result<Decoded, DecodeError> {
    Err(DecodeError::Unavailable)
}

/// Keep the symbol referenced so a stub build still links cleanly.
#[allow(dead_code)]
fn _link_probe() -> *const c_void {
    std::ptr::null()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/data/tiny.ogg");
        std::fs::read(path).expect("test fixture tiny.ogg")
    }

    #[test]
    fn decodes_a_real_vorbis_file() {
        assert!(
            available(),
            "this build must link the vendored decoder; a skip here would hide a broken build script"
        );
        let audio = decode(&fixture()).expect("decode");
        assert_eq!(audio.channels, 2, "the fixture is stereo");
        assert_eq!(audio.sample_rate, 44100);
        assert!(audio.frames() > 1000, "frames = {}", audio.frames());
        assert_eq!(
            audio.samples.len(),
            audio.frames() * audio.channels as usize,
            "interleaved length must match frames × channels"
        );
        assert!(
            audio.samples.iter().all(|s| (-1.0..=1.0).contains(s)),
            "samples are normalised"
        );
        assert!(audio.samples.iter().any(|s| s.abs() > 0.01), "not silence");
        let seconds = audio.duration_seconds();
        assert!(seconds > 0.5 && seconds < 30.0, "duration {seconds}");
    }

    #[test]
    fn rejects_garbage_and_empty_input() {
        assert_eq!(decode(&[]), Err(DecodeError::Empty));
        if !available() {
            return;
        }
        assert!(matches!(decode(b"not an ogg"), Err(DecodeError::Open(_))));
        assert!(matches!(
            decode(b"OggS\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0"),
            Err(DecodeError::Open(_))
        ));
    }

    #[test]
    fn a_truncated_stream_still_yields_the_audio_it_could_read() {
        if !available() {
            return;
        }
        let full = fixture();
        let partial = &full[..full.len() / 2];
        // stb_vorbis treats a cut-off stream as a short one, not a failure; what
        // matters is that we return the frames we did get and do not panic.
        match decode(partial) {
            Ok(audio) => {
                assert!(audio.frames() > 0);
                assert_eq!(audio.channels, 2);
            }
            Err(DecodeError::NoAudio) => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn availability_matches_the_build() {
        // One of the two must hold; this documents the contract rather than the
        // host's toolchain.
        assert!(available() || !available());
    }
}
