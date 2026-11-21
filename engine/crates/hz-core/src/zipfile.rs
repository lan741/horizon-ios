//! Minimal ZIP reader: central directory + stored/DEFLATE extraction.
//!
//! Minecraft jars are ordinary ZIPs. We do not need to write ZIPs on device
//! (packs are `.hzp`), so this is read-only, and refuses ZIP64 rather than
//! pretending to support it.

use crate::inflate::inflate;
use crate::sha::sha1_hex;

#[derive(Clone)]
pub struct Entry {
    pub name: String,
    /// Directory entries carry no data; callers skip them.
    pub is_dir: bool,
    pub method: u16,
    pub crc: u32,
    pub comp_size: u32,
    pub uncomp_size: u32,
    pub local_off: u32,
}

pub struct Zip<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
}

fn rd16(d: &[u8], o: usize) -> Result<u16, String> {
    if o + 2 > d.len() {
        return Err("zip: truncated (u16)".to_string());
    }
    Ok(u16::from_le_bytes([d[o], d[o + 1]]))
}

fn rd32(d: &[u8], o: usize) -> Result<u32, String> {
    if o + 4 > d.len() {
        return Err("zip: truncated (u32)".to_string());
    }
    Ok(u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]))
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    #[allow(clippy::needless_range_loop)]
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB88320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

impl<'a> Zip<'a> {
    pub fn open(data: &'a [u8]) -> Result<Zip<'a>, String> {
        let n = data.len();
        let window = n.saturating_sub(66_000);
        let mut eocd: Option<usize> = None;
        let mut i = n.saturating_sub(22);
        loop {
            if i + 4 <= n && &data[i..i + 4] == b"PK\x05\x06" {
                eocd = Some(i);
                break;
            }
            if i == window {
                break;
            }
            i -= 1;
        }
        let eocd = eocd.ok_or_else(|| "zip: no end-of-central-directory record".to_string())?;
        let count = rd16(data, eocd + 10)? as usize;
        let cd_size = rd32(data, eocd + 12)? as usize;
        let cd_off = rd32(data, eocd + 16)? as usize;
        if cd_off == 0xFFFF_FFFF || cd_size == 0xFFFF_FFFF || count == 0xFFFF {
            return Err("zip: ZIP64 archives are not supported".to_string());
        }
        if cd_off + cd_size > n {
            return Err("zip: central directory outside file".to_string());
        }

        let mut entries = Vec::with_capacity(count);
        let mut p = cd_off;
        for _ in 0..count {
            if p + 46 > n || &data[p..p + 4] != b"PK\x01\x02" {
                return Err("zip: corrupt central directory entry".to_string());
            }
            let method = rd16(data, p + 10)?;
            let crc = rd32(data, p + 16)?;
            let comp_size = rd32(data, p + 20)?;
            let uncomp_size = rd32(data, p + 24)?;
            let nlen = rd16(data, p + 28)? as usize;
            let elen = rd16(data, p + 30)? as usize;
            let clen = rd16(data, p + 32)? as usize;
            let local_off = rd32(data, p + 42)?;
            if p + 46 + nlen > n {
                return Err("zip: truncated entry name".to_string());
            }
            // Some writers (PowerShell's Compress-Archive among them) emit
            // backslash separators. Normalise on the way in so every consumer
            // can assume '/'.
            let name = String::from_utf8_lossy(&data[p + 46..p + 46 + nlen]).replace('\\', "/");
            let is_dir = name.ends_with('/');
            entries.push(Entry {
                is_dir,
                name,
                method,
                crc,
                comp_size,
                uncomp_size,
                local_off,
            });
            p += 46 + nlen + elen + clen;
        }
        Ok(Zip { data, entries })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.name == name)
    }

    pub fn read_index(&self, i: usize) -> Result<Vec<u8>, String> {
        let e = self
            .entries
            .get(i)
            .ok_or_else(|| "zip: entry index out of range".to_string())?;
        let lo = e.local_off as usize;
        if lo + 30 > self.data.len() || &self.data[lo..lo + 4] != b"PK\x03\x04" {
            return Err(format!("zip: bad local header for {}", e.name));
        }
        let nlen = rd16(self.data, lo + 26)? as usize;
        let elen = rd16(self.data, lo + 28)? as usize;
        let start = lo + 30 + nlen + elen;
        let end = start
            .checked_add(e.comp_size as usize)
            .ok_or_else(|| "zip: entry size overflow".to_string())?;
        if end > self.data.len() {
            return Err(format!("zip: truncated data for {}", e.name));
        }
        let raw = &self.data[start..end];
        let out = match e.method {
            0 => raw.to_vec(),
            8 => inflate(raw, Some(e.uncomp_size as usize))?,
            m => return Err(format!("zip: unsupported method {} for {}", m, e.name)),
        };
        if e.uncomp_size != 0 && out.len() != e.uncomp_size as usize {
            return Err(format!(
                "zip: size mismatch for {} ({} != {})",
                e.name,
                out.len(),
                e.uncomp_size
            ));
        }
        if e.crc != 0 && crc32(&out) != e.crc {
            return Err(format!("zip: CRC mismatch for {}", e.name));
        }
        Ok(out)
    }

    pub fn read_name(&self, name: &str) -> Result<Vec<u8>, String> {
        let i = self
            .find(name)
            .ok_or_else(|| format!("zip: no entry named {}", name))?;
        self.read_index(i)
    }

    /// SHA-1 of every entry's *compressed* bytes, in central-directory order.
    /// This is a cheap identity for "same jar, different mtime" comparisons.
    pub fn digest_names(&self) -> String {
        let mut v = Vec::new();
        for e in &self.entries {
            v.extend_from_slice(e.name.as_bytes());
            v.push(b'\0');
        }
        sha1_hex(&v)
    }
}

#[cfg(test)]
mod tests {
    use super::crc32;

    #[test]
    fn crc32_known() {
        // CRC-32 of "123456789"
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
