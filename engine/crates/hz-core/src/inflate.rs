//! Raw DEFLATE (RFC 1951) decoder.
//!
//! Minecraft's jars, `.mrpack` archives and Mojang's asset zips are all
//! DEFLATE streams, and a launcher may not spawn a subprocess on iOS. So the
//! decoder lives here, written once, tested against real archives.
//!
//! Decoding is bit-serial (puff.c style): clear, small, no unsafe, no tables
//! that depend on endianness. Throughput is roughly 20–40 MB/s, which is more
//! than enough for a one-time import of a ~40 MB jar.

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    buf: u64,
    cnt: u32,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8]) -> Self {
        Bits {
            d,
            pos: 0,
            buf: 0,
            cnt: 0,
        }
    }

    fn bit(&mut self) -> Result<u32, String> {
        if self.cnt == 0 {
            if self.pos >= self.d.len() {
                return Err("deflate: unexpected end of stream".to_string());
            }
            self.buf = self.d[self.pos] as u64;
            self.pos += 1;
            self.cnt = 8;
        }
        let b = (self.buf & 1) as u32;
        self.buf >>= 1;
        self.cnt -= 1;
        Ok(b)
    }

    fn bits(&mut self, n: u32) -> Result<u32, String> {
        let mut v = 0u32;
        for i in 0..n {
            v |= self.bit()? << i;
        }
        Ok(v)
    }

    fn align(&mut self) {
        self.cnt = 0;
        self.buf = 0;
    }

    fn raw_byte(&mut self) -> Result<u8, String> {
        if self.cnt != 0 {
            return Err("deflate: byte read while unaligned".to_string());
        }
        if self.pos >= self.d.len() {
            return Err("deflate: unexpected end of stored block".to_string());
        }
        let b = self.d[self.pos];
        self.pos += 1;
        Ok(b)
    }
}

struct Huff {
    counts: [u16; 16],
    syms: Vec<u16>,
}

impl Huff {
    fn build(lengths: &[u8]) -> Result<Huff, String> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            if l as usize > 15 {
                return Err("deflate: code length above 15".to_string());
            }
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offs = [0u16; 16];
        let mut sum: u16 = 0;
        for i in 1..16 {
            offs[i] = sum;
            sum += counts[i];
        }
        let mut syms = vec![0u16; sum as usize];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                let o = &mut offs[l as usize];
                if (*o as usize) >= syms.len() {
                    return Err("deflate: symbol overflow".to_string());
                }
                syms[*o as usize] = sym as u16;
                *o += 1;
            }
        }
        Ok(Huff { counts, syms })
    }

    fn decode(&self, br: &mut Bits<'_>) -> Result<u16, String> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16 {
            code |= br.bit()? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                let idx = index + (code - first);
                if idx < 0 || idx as usize >= self.syms.len() {
                    return Err("deflate: symbol index out of range".to_string());
                }
                return Ok(self.syms[idx as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("deflate: invalid huffman code".to_string())
    }
}

const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn fixed_tables() -> Result<(Huff, Huff), String> {
    let mut lit = [0u8; 288];
    for (i, l) in lit.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let dist = [5u8; 32];
    Ok((Huff::build(&lit)?, Huff::build(&dist)?))
}

fn dynamic_tables(br: &mut Bits<'_>) -> Result<(Huff, Huff), String> {
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;
    if hlit > 288 || hdist > 32 {
        return Err("deflate: bad dynamic header".to_string());
    }
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let mut cl = [0u8; 19];
    for i in 0..hclen {
        cl[ORDER[i]] = br.bits(3)? as u8;
    }
    let clh = Huff::build(&cl)?;

    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0usize;
    while i < lengths.len() {
        let sym = clh.decode(br)?;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                if i == 0 {
                    return Err("deflate: repeat with no previous length".to_string());
                }
                let prev = lengths[i - 1];
                let rep = 3 + br.bits(2)? as usize;
                for _ in 0..rep {
                    if i >= lengths.len() {
                        return Err("deflate: length repeat overflow".to_string());
                    }
                    lengths[i] = prev;
                    i += 1;
                }
            }
            17 => {
                let rep = 3 + br.bits(3)? as usize;
                i += rep;
            }
            18 => {
                let rep = 11 + br.bits(7)? as usize;
                i += rep;
            }
            _ => return Err("deflate: bad code-length symbol".to_string()),
        }
    }
    if i > lengths.len() {
        return Err("deflate: code lengths overshoot".to_string());
    }
    let lit = Huff::build(&lengths[..hlit])?;
    let dist = Huff::build(&lengths[hlit..hlit + hdist])?;
    Ok((lit, dist))
}

/// Inflate a raw DEFLATE stream. `expected` is a size hint only.
pub fn inflate(data: &[u8], expected: Option<usize>) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::with_capacity(expected.unwrap_or(data.len() * 3 + 64));
    let mut br = Bits::new(data);
    loop {
        let last = br.bit()?;
        let btype = br.bits(2)?;
        match btype {
            0 => {
                br.align();
                let len = br.raw_byte()? as usize | ((br.raw_byte()? as usize) << 8);
                let nlen = br.raw_byte()? as usize | ((br.raw_byte()? as usize) << 8);
                if len ^ 0xffff != nlen {
                    return Err("deflate: stored block length mismatch".to_string());
                }
                for _ in 0..len {
                    out.push(br.raw_byte()?);
                }
            }
            1 | 2 => {
                let (lit, dist) = if btype == 1 {
                    fixed_tables()?
                } else {
                    dynamic_tables(&mut br)?
                };
                loop {
                    let sym = lit.decode(&mut br)?;
                    if sym < 256 {
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let li = (sym - 257) as usize;
                        if li >= LBASE.len() {
                            return Err("deflate: bad length symbol".to_string());
                        }
                        let len = LBASE[li] as usize + br.bits(LEXT[li] as u32)? as usize;
                        let di = dist.decode(&mut br)? as usize;
                        if di >= DBASE.len() {
                            return Err("deflate: bad distance symbol".to_string());
                        }
                        let distance = DBASE[di] as usize + br.bits(DEXT[di] as u32)? as usize;
                        if distance > out.len() {
                            return Err("deflate: distance beyond output".to_string());
                        }
                        if let Some(max) = expected {
                            if out.len() + len > max.saturating_mul(64) + (1 << 20) {
                                return Err("deflate: output far larger than declared".to_string());
                            }
                        }
                        let start = out.len() - distance;
                        for k in 0..len {
                            let b = out[start + k];
                            out.push(b);
                        }
                    }
                }
            }
            _ => return Err("deflate: reserved block type".to_string()),
        }
        if last == 1 {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::inflate;

    #[test]
    fn stored_block() {
        // BFINAL=1, BTYPE=00, len=5, nlen=~5, "hello"
        let data = [0x01, 0x05, 0x00, 0xfa, 0xff, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(inflate(&data, Some(5)).unwrap(), b"hello");
    }

    #[test]
    fn rejects_garbage() {
        assert!(inflate(&[0xff, 0xff, 0xff], None).is_err());
    }
}
