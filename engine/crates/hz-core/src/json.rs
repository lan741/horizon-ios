//! Small JSON reader/writer.
//!
//! Minecraft's data files are JSON; on device we only need to *read* them to
//! compile native descriptors. Object order is preserved (Vec of pairs) so a
//! round-trip of a manifest stays byte-stable where it matters.

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn parse(s: &str) -> Result<Json, String> {
        // A leading UTF-8 BOM shows up in files written by Windows editors and
        // by PowerShell's Set-Content; Mojang's own assets are BOM-free but
        // resource packs are not always.
        let s = s.strip_prefix('\u{feff}').unwrap_or(s);
        let mut p = Parser {
            b: s.as_bytes(),
            i: 0,
        };
        p.ws();
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("json: trailing data at byte {}", p.i));
        }
        Ok(v)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_obj(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(o) => Some(o),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        self.as_obj()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub fn to_json_string(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 9.0e15 {
                    out.push_str(&format!("{}", *n as i64));
                } else {
                    out.push_str(&format!("{}", n));
                }
            }
            Json::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Json::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(o) => {
                out.push('{');
                for (i, (k, v)) in o.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    Json::Str(k.clone()).write(out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("json: expected '{}' at byte {}", c as char, self.i))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => {
                self.lit(b"true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.lit(b"false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.lit(b"null")?;
                Ok(Json::Null)
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            other => Err(format!("json: unexpected {:?} at byte {}", other, self.i)),
        }
    }

    fn lit(&mut self, want: &[u8]) -> Result<(), String> {
        if self.b[self.i..].starts_with(want) {
            self.i += want.len();
            Ok(())
        } else {
            Err(format!("json: bad literal at byte {}", self.i))
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == b'.' || c == b'e' || c == b'E' || c == b'+' || c == b'-' {
                self.i += 1;
            } else {
                break;
            }
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "json: non-utf8 number")?;
        let n: f64 = s.parse().map_err(|_| format!("json: bad number '{}'", s))?;
        Ok(Json::Num(n))
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let c = self
                .peek()
                .ok_or_else(|| "json: unterminated string".to_string())?;
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let e = self
                        .peek()
                        .ok_or_else(|| "json: unterminated escape".to_string())?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                // surrogate pair
                                if self.peek() == Some(b'\\') {
                                    self.i += 1;
                                    if self.peek() == Some(b'u') {
                                        self.i += 1;
                                        let lo = self.hex4()?;
                                        let v = 0x10000
                                            + ((cp - 0xD800) << 10)
                                            + (lo.wrapping_sub(0xDC00) & 0x3ff);
                                        out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                                        continue;
                                    }
                                }
                                out.push('\u{fffd}');
                            } else {
                                out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                            }
                        }
                        other => out.push(other as char),
                    }
                }
                _ => {
                    // multi-byte UTF-8: copy continuation bytes
                    if c < 0x80 {
                        out.push(c as char);
                    } else {
                        let len = if c >> 5 == 0b110 {
                            2
                        } else if c >> 4 == 0b1110 {
                            3
                        } else if c >> 3 == 0b11110 {
                            4
                        } else {
                            return Err("json: invalid utf-8 lead byte".to_string());
                        };
                        if self.i - 1 + len > self.b.len() {
                            return Err("json: truncated utf-8".to_string());
                        }
                        let s = std::str::from_utf8(&self.b[self.i - 1..self.i - 1 + len])
                            .map_err(|_| "json: invalid utf-8".to_string())?;
                        out.push_str(s);
                        self.i += len - 1;
                    }
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        if self.i + 4 > self.b.len() {
            return Err("json: truncated \\u escape".to_string());
        }
        let s = std::str::from_utf8(&self.b[self.i..self.i + 4])
            .map_err(|_| "json: bad \\u escape".to_string())?;
        let v = u32::from_str_radix(s, 16).map_err(|_| "json: bad hex digit".to_string())?;
        self.i += 4;
        Ok(v)
    }

    fn array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        let mut v = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(v));
        }
        loop {
            self.ws();
            v.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(v));
                }
                _ => return Err(format!("json: expected , or ] at byte {}", self.i)),
            }
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut v = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(v));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.ws();
            self.expect(b':')?;
            self.ws();
            let val = self.value()?;
            v.push((k, val));
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(v));
                }
                _ => return Err(format!("json: expected , or }} at byte {}", self.i)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Json;

    #[test]
    fn parses_and_roundtrips() {
        let src = r#"{"a":1,"b":[true,null,"x\u00e9\n"],"c":{"d":-2.5}}"#;
        let v = Json::parse(src).unwrap();
        assert_eq!(v.get("a").unwrap().as_f64(), Some(1.0));
        assert_eq!(v.get("b").unwrap().as_arr().unwrap().len(), 3);
        let round = Json::parse(&v.to_json_string()).unwrap();
        assert_eq!(v, round);
    }

    #[test]
    fn rejects_trailing() {
        assert!(Json::parse("{} x").is_err());
    }
}
