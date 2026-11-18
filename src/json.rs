//! Minimal JSON value + parser (no new crate dependency: the whitelist is serde + toml).
//!
//! Only what the codebase needs: objects, arrays, strings (with escapes, including `\uXXXX`
//! surrogate pairs), numbers (kept as f64), booleans and null. Used for CMake's File API
//! codemodel replies and for MSVC's `/sourceDependencies` JSON dep files. Errors are plain
//! strings naming the byte offset.

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Object member lookup by key (first match wins); None for non-objects or missing keys.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }

}

/// Parse a whole JSON document; trailing garbage after the top-level value is an error.
pub fn parse(text: &str) -> Result<Json, String> {
    let bytes = text.as_bytes();
    let mut p = Parser { bytes, pos: 0 };
    p.skip_ws();
    let value = p.value()?;
    p.skip_ws();
    if p.pos != bytes.len() {
        return Err(format!("trailing data after JSON value at byte {}", p.pos));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let b = self.peek();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, ch: u8) -> Result<(), String> {
        match self.next() {
            Some(b) if b == ch => Ok(()),
            Some(b) => Err(format!(
                "expected '{}' at byte {}, found '{}'",
                ch as char, self.pos - 1, b as char
            )),
            None => Err(format!("expected '{}' at end of input", ch as char)),
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-') | Some(b'0'..=b'9') => self.number(),
            Some(b) => Err(format!("unexpected '{}' at byte {}", b as char, self.pos)),
            None => Err("unexpected end of input".into()),
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(format!("invalid literal at byte {}", self.pos))
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')) {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| format!("invalid number at byte {start}"))?;
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("invalid number {text:?} at byte {start}"))
    }

    fn object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut pairs = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Obj(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(format!("expected object key at byte {}", self.pos));
            }
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let value = self.value()?;
            pairs.push((key, value));
            self.skip_ws();
            match self.next() {
                Some(b',') => {}
                Some(b'}') => break,
                Some(b) => {
                    return Err(format!("expected ',' or '}}' at byte {}, found '{}'", self.pos - 1, b as char))
                }
                None => return Err("unterminated object".into()),
            }
        }
        Ok(Json::Obj(pairs))
    }

    fn array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            match self.next() {
                Some(b',') => {}
                Some(b']') => break,
                Some(b) => {
                    return Err(format!("expected ',' or ']' at byte {}, found '{}'", self.pos - 1, b as char))
                }
                None => return Err("unterminated array".into()),
            }
        }
        Ok(Json::Arr(items))
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let b = match self.next() {
                Some(b) => b,
                None => return Err("unterminated string".into()),
            };
            match b {
                b'"' => break,
                b'\\' => {
                    let esc = self.next().ok_or("unterminated escape")?;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                // High surrogate: a second \uXXXX low surrogate must follow.
                                if self.next() != Some(b'\\') || self.next() != Some(b'u') {
                                    return Err(format!(
                                        "unpaired high surrogate at byte {}",
                                        self.pos
                                    ));
                                }
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(format!(
                                        "unpaired high surrogate at byte {}",
                                        self.pos
                                    ));
                                }
                                let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                out.push(char::from_u32(c).ok_or("invalid code point")?);
                            } else if (0xDC00..0xE000).contains(&cp) {
                                return Err(format!("lone low surrogate at byte {}", self.pos));
                            } else {
                                out.push(char::from_u32(cp).ok_or("invalid code point")?);
                            }
                        }
                        _ => {
                            return Err(format!(
                                "invalid escape '\\{}' at byte {}",
                                esc as char,
                                self.pos - 1
                            ))
                        }
                    }
                }
                0x00..=0x1F => {
                    return Err(format!("unescaped control character at byte {}", self.pos - 1))
                }
                _ => {
                    // Copy one UTF-8 sequence verbatim (the input is &str, so it is valid UTF-8).
                    let start = self.pos - 1;
                    let len = utf8_len(b);
                    out.push_str(
                        std::str::from_utf8(&self.bytes[start..start + len])
                            .map_err(|_| format!("invalid UTF-8 at byte {start}"))?,
                    );
                    self.pos = start + len;
                }
            }
        }
        Ok(out)
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut value: u32 = 0;
        for _ in 0..4 {
            let b = self.next().ok_or("truncated \\u escape")?;
            let digit = (b as char).to_digit(16).ok_or_else(|| {
                format!("invalid hex digit '{}' at byte {}", b as char, self.pos - 1)
            })?;
            value = value * 16 + digit;
        }
        Ok(value)
    }
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars_and_nesting() {
        let v = parse(r#"{"a": [1, 2.5, -3e2, true, false, null], "b": {"c": "x\ny"}}"#)
            .unwrap();
        assert_eq!(v.get("a").unwrap().as_arr().unwrap().len(), 6);
        assert_eq!(
            v.get("b").unwrap().get("c").unwrap().as_str(),
            Some("x\ny")
        );
        assert!(matches!(v.get("a").unwrap().as_arr().unwrap()[2], Json::Num(n) if n == -300.0));
    }

    #[test]
    fn parses_unicode_escapes_including_surrogate_pairs() {
        let v = parse(r#""caf\u00e9 \ud83d\ude00""#).unwrap();
        assert_eq!(v.as_str(), Some("café 😀"));
        assert!(parse(r#""\ud800""#).is_err());
        assert!(parse(r#""\udc00""#).is_err());
    }

    #[test]
    fn rejects_trailing_data_and_malformed_input() {
        assert!(parse("{} x").is_err());
        assert!(parse("{").is_err());
        assert!(parse("[1,]").is_err());
        assert!(parse(r#""abc"#).is_err());
        assert!(parse("1e").is_err());
        assert!(parse("-").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn parses_codemodel_shaped_fragment() {
        let text = r#"{
          "configurations": [{"name": "Debug", "targets": [
            {"name": "gtest", "id": "gtest::@a1", "type": "STATIC_LIBRARY",
             "artifacts": [{"path": "lib/libgtest.a"}],
             "link": {"language": "CXX", "libraries": [{"targetId": "pthread::@b2"}]},
             "compileGroups": [{"language": "CXX", "includes": [{"path": "/x/include"}],
              "defines": [{"define": "GTEST_HAS_PTHREAD=1"}]}]}
          ]}]
        }"#;
        let v = parse(text).unwrap();
        let target = v
            .get("configurations")
            .unwrap()
            .as_arr()
            .unwrap()
            .first()
            .unwrap()
            .get("targets")
            .unwrap()
            .as_arr()
            .unwrap()
            .first()
            .unwrap();
        assert_eq!(target.get("name").unwrap().as_str(), Some("gtest"));
        assert_eq!(
            target
                .get("artifacts")
                .unwrap()
                .as_arr()
                .unwrap()
                .first()
                .unwrap()
                .get("path")
                .unwrap()
                .as_str(),
            Some("lib/libgtest.a")
        );
    }

    #[test]
    fn parses_source_dependencies_shaped_fragment() {
        let v = parse(r#"{"Data": {"Source": "/p/main.cpp", "Includes": ["/p/a.h", "/p/b.h"]}}"#)
            .unwrap();
        let includes = v
            .get("Data")
            .unwrap()
            .get("Includes")
            .unwrap()
            .as_arr()
            .unwrap();
        assert_eq!(includes.len(), 2);
        assert_eq!(includes[0].as_str(), Some("/p/a.h"));
    }
}
