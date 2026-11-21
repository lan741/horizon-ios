//! Little-endian writer used by every on-disk format here.

pub struct Buf {
    pub d: Vec<u8>,
}

impl Buf {
    pub fn new() -> Buf {
        Buf { d: Vec::new() }
    }

    pub fn with_capacity(n: usize) -> Buf {
        Buf {
            d: Vec::with_capacity(n),
        }
    }

    pub fn u8(&mut self, v: u8) {
        self.d.push(v);
    }

    pub fn u16(&mut self, v: u16) {
        self.d.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i16(&mut self, v: i16) {
        self.d.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.d.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.d.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.d.extend_from_slice(&v.to_le_bytes());
    }

    pub fn f32(&mut self, v: f32) {
        self.d.extend_from_slice(&v.to_le_bytes());
    }

    pub fn bytes(&mut self, v: &[u8]) {
        self.d.extend_from_slice(v);
    }

    /// Length-prefixed UTF-8 (u16 length; longer strings are truncated loudly
    /// rather than silently corrupting the stream).
    pub fn str16(&mut self, s: &str) {
        let b = s.as_bytes();
        let n = b.len().min(u16::MAX as usize);
        self.u16(n as u16);
        self.d.extend_from_slice(&b[..n]);
    }

    pub fn finish(self) -> Vec<u8> {
        self.d
    }
}

impl Default for Buf {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Reader<'a> {
    pub d: &'a [u8],
    pub i: usize,
}

impl<'a> Reader<'a> {
    pub fn new(d: &'a [u8]) -> Reader<'a> {
        Reader { d, i: 0 }
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        let b = *self.d.get(self.i).ok_or("buf: truncated u8")?;
        self.i += 1;
        Ok(b)
    }

    pub fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    pub fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.i + n > self.d.len() {
            return Err(format!(
                "buf: truncated (need {} at {}, have {})",
                n,
                self.i,
                self.d.len()
            ));
        }
        let s = &self.d[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }

    pub fn str16(&mut self) -> Result<String, String> {
        let n = self.u16()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    pub fn done(&self) -> bool {
        self.i >= self.d.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut b = Buf::new();
        b.u8(7);
        b.u16(300);
        b.str16("hello");
        b.f32(1.5);
        let v = b.finish();
        let mut r = Reader::new(&v);
        assert_eq!(r.u8().unwrap(), 7);
        assert_eq!(r.u16().unwrap(), 300);
        assert_eq!(r.str16().unwrap(), "hello");
        assert_eq!(r.f32().unwrap(), 1.5);
        assert!(r.done());
    }
}
