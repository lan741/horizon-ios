//! Ogg container parsing and the Vorbis identification header.
//!
//! Minecraft ships sounds as Ogg Vorbis. iOS's AVFoundation cannot decode that
//! format, and writing a Vorbis decoder (floor/residue/codebook/MDCT) is a
//! project of its own, so this module does what is both useful and honest:
//!
//! * validates the Ogg page structure and reassembles packets,
//! * reads the Vorbis identification header (channels, sample rate, bitrates,
//!   block sizes),
//! * reports the stream duration from the final granule position.
//!
//! That is enough for the app to say precisely what a sound is, to stream
//! packets to a decoder if one is added later, and to fail loudly instead of
//! playing silence.
//!
//! Codes and page layout: RFC 3533 (Ogg) and the Vorbis I specification.

/// Ogg's CRC-32: polynomial 0x04c11db7, no reflection, zero initial value, no
/// final inversion. Different from the ZIP/PNG CRC on purpose.
pub fn ogg_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for byte in data {
        crc ^= (*byte as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VorbisInfo {
    pub channels: u8,
    pub sample_rate: u32,
    pub version: u32,
    pub bitrate_nominal: i32,
    pub bitrate_max: i32,
    pub bitrate_min: i32,
    pub block_size_small: u16,
    pub block_size_large: u16,
}

impl VorbisInfo {
    /// Nominal bitrate, or 0 when the header did not carry one.
    pub fn nominal_or_default(&self) -> i32 {
        if self.bitrate_nominal > 0 {
            self.bitrate_nominal
        } else {
            0
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OggInfo {
    pub vorbis: Option<VorbisInfo>,
    /// Number of packets found across all pages of the first logical stream.
    pub packets: usize,
    /// Granule position of the last page seen, in samples.
    pub last_granule: u64,
    /// True when every page checksum matched.
    pub checksums_ok: bool,
    pub pages: usize,
    pub bytes: usize,
}

impl OggInfo {
    /// Duration in seconds, from the final granule and the sample rate.
    pub fn duration_seconds(&self) -> f32 {
        match &self.vorbis {
            Some(v) if v.sample_rate > 0 => self.last_granule as f32 / v.sample_rate as f32,
            _ => 0.0,
        }
    }

    /// Human-readable codec name for diagnostics.
    pub fn codec(&self) -> &'static str {
        if self.vorbis.is_some() {
            "vorbis"
        } else {
            "ogg-unknown"
        }
    }
}

struct Reader<'a> {
    d: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.d.get(self.i)?;
        self.i += 1;
        Some(v)
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.d.get(self.i..self.i + 4)?;
        self.i += 4;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }
}

/// Parse an Ogg file far enough to describe it.
pub fn parse(bytes: &[u8]) -> Result<OggInfo, String> {
    if bytes.len() < 27 {
        return Err("ogg: too short for a page header".to_string());
    }
    let mut offset = 0usize;
    let mut pages = 0usize;
    let mut packets = 0usize;
    let mut last_granule = 0u64;
    let mut checksums_ok = true;
    let mut first_packet: Option<Vec<u8>> = None;

    let mut partial: Vec<u8> = Vec::new();

    while offset + 27 <= bytes.len() {
        if &bytes[offset..offset + 4] != b"OggS" {
            return Err(format!("ogg: no capture pattern at byte {}", offset));
        }
        let version = bytes[offset + 4];
        if version != 0 {
            return Err(format!(
                "ogg: unsupported stream structure version {}",
                version
            ));
        }
        let header_type = bytes[offset + 5];
        let granule = {
            let b = &bytes[offset + 6..offset + 14];
            u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
        };
        let stored_crc = {
            let b = &bytes[offset + 22..offset + 26];
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        };
        let segments = bytes[offset + 26] as usize;
        let table_start = offset + 27;
        if table_start + segments > bytes.len() {
            return Err("ogg: truncated segment table".to_string());
        }
        let lacing = &bytes[table_start..table_start + segments];
        let payload_len: usize = lacing.iter().map(|v| *v as usize).sum();
        let payload_start = table_start + segments;
        if payload_start + payload_len > bytes.len() {
            return Err("ogg: truncated page payload".to_string());
        }

        // Checksum covers the whole page with the CRC field zeroed.
        let mut copy = bytes[offset..payload_start + payload_len].to_vec();
        copy[22..26].fill(0);
        if ogg_crc32(&copy) != stored_crc {
            checksums_ok = false;
        }

        // Reassemble packets: a lacing value < 255 terminates a packet.
        let mut cursor = payload_start;
        for value in lacing {
            let n = *value as usize;
            partial.extend_from_slice(&bytes[cursor..cursor + n]);
            cursor += n;
            if n < 255 {
                if first_packet.is_none() {
                    first_packet = Some(partial.clone());
                }
                packets += 1;
                partial.clear();
            }
        }

        pages += 1;
        if header_type & 0x04 != 0 || granule != u64::MAX {
            last_granule = granule;
        }
        offset = payload_start + payload_len;
    }

    if pages == 0 {
        return Err("ogg: no pages found".to_string());
    }

    let vorbis = first_packet.as_deref().and_then(parse_vorbis_id);
    Ok(OggInfo {
        vorbis,
        packets,
        last_granule,
        checksums_ok,
        pages,
        bytes: bytes.len(),
    })
}

/// Parse the Vorbis identification header (packet type 1).
pub fn parse_vorbis_id(packet: &[u8]) -> Option<VorbisInfo> {
    let mut r = Reader { d: packet, i: 0 };
    if r.u8()? != 0x01 {
        return None;
    }
    if r.d.get(1..7)? != b"vorbis" {
        return None;
    }
    r.i = 7;
    let version = r.u32()?;
    let channels = r.u8()?;
    let sample_rate = r.u32()?;
    let bitrate_max = r.i32()?;
    let bitrate_nominal = r.i32()?;
    let bitrate_min = r.i32()?;
    let blocksizes = r.u8()?;
    let small = 1u16 << (blocksizes & 0x0f);
    let large = 1u16 << (blocksizes >> 4);
    let framing = r.u8()?;
    if framing & 1 == 0 {
        return None;
    }
    Some(VorbisInfo {
        channels,
        sample_rate,
        version,
        bitrate_nominal,
        bitrate_max,
        bitrate_min,
        block_size_small: small,
        block_size_large: large,
    })
}

/// Page and header builders used by this crate's tests and by `hz-game`'s.
/// Public so a dependant can construct a real container without a fixture file.
pub mod testutil {
    use super::ogg_crc32;

    /// Build one Ogg page from packets, with a correct checksum.
    pub fn page(
        serial: u32,
        sequence: u32,
        header_type: u8,
        granule: u64,
        packets: &[&[u8]],
    ) -> Vec<u8> {
        let mut lacing: Vec<u8> = Vec::new();
        let mut payload: Vec<u8> = Vec::new();
        for packet in packets {
            let mut remaining = packet.len();
            while remaining >= 255 {
                lacing.push(255);
                remaining -= 255;
            }
            lacing.push(remaining as u8);
            payload.extend_from_slice(packet);
        }
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"OggS");
        out.push(0);
        out.push(header_type);
        out.extend_from_slice(&granule.to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(&sequence.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // checksum placeholder
        out.push(lacing.len() as u8);
        out.extend_from_slice(&lacing);
        out.extend_from_slice(&payload);
        let crc = ogg_crc32(&out);
        out[22..26].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// A minimal Vorbis identification packet.
    pub fn vorbis_id(channels: u8, sample_rate: u32) -> Vec<u8> {
        let mut p = vec![0x01];
        p.extend_from_slice(b"vorbis");
        p.extend_from_slice(&0u32.to_le_bytes()); // version
        p.push(channels);
        p.extend_from_slice(&sample_rate.to_le_bytes());
        p.extend_from_slice(&(-1i32).to_le_bytes()); // max
        p.extend_from_slice(&128000i32.to_le_bytes()); // nominal
        p.extend_from_slice(&(-1i32).to_le_bytes()); // min
        p.push(0xb8); // blocksize: 256 small / 2048 large
        p.push(1); // framing
        p
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::{page, vorbis_id};
    use super::*;

    #[test]
    fn parses_a_vorbis_ident_header() {
        let packet = vorbis_id(2, 44100);
        let info = parse_vorbis_id(&packet).expect("ident header");
        assert_eq!(info.channels, 2);
        assert_eq!(info.sample_rate, 44100);
        assert_eq!(info.bitrate_nominal, 128000);
        assert_eq!(info.block_size_small, 256);
        assert_eq!(info.block_size_large, 2048);
    }

    #[test]
    fn rejects_non_vorbis_first_packet() {
        let not_vorbis = b"\x01nope\0\0\0\0".to_vec();
        assert!(parse_vorbis_id(&not_vorbis).is_none());
    }

    #[test]
    fn reassembles_packets_across_pages() {
        let id = vorbis_id(1, 22050);
        let comment = b"\x03vorbis-hello".to_vec();
        let setup = vec![0x05u8; 600]; // spans three lacing values
        let mut bytes = page(7, 0, 0x02, 0, &[&id]);
        bytes.extend(page(7, 1, 0x00, 0, &[&comment, &setup]));
        bytes.extend(page(7, 2, 0x04, 22050, &[b"audio"]));

        let info = parse(&bytes).expect("parses");
        assert_eq!(info.pages, 3);
        // id + comment + the 600-byte setup (one packet across three lacing
        // values) + audio
        assert_eq!(info.packets, 4);
        assert!(info.checksums_ok);
        let duration = info.duration_seconds();
        let codec = info.codec();
        let v = info.vorbis.expect("ident header");
        assert_eq!(v.sample_rate, 22050);
        assert_eq!(v.channels, 1);
        assert!((duration - 1.0).abs() < 1e-3);
        assert_eq!(codec, "vorbis");
    }

    #[test]
    fn reports_a_bad_checksum_instead_of_failing() {
        let id = vorbis_id(1, 8000);
        let mut bytes = page(1, 0, 0x02, 0, &[&id]);
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff; // corrupt the payload, not the CRC
        let info = parse(&bytes).expect("structure is still fine");
        assert!(!info.checksums_ok, "the checksum must not match");
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        assert!(parse(b"not an ogg stream").is_err());
        let id = vorbis_id(1, 8000);
        let bytes = page(1, 0, 0x02, 0, &[&id]);
        assert!(parse(&bytes[..bytes.len() - 5]).is_err());
    }

    #[test]
    fn crc_is_the_ogg_variant() {
        // Ogg's CRC leaves the initial value at zero and does not invert.
        assert_eq!(ogg_crc32(&[]), 0);
        assert_ne!(ogg_crc32(b"OggS"), 0);
    }
}
