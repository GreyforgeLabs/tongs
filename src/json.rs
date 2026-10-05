//! A JSON reader and writer that behave like CPython's `json` module.
//!
//! tongs files must stay interchangeable with the Python 1.x implementation
//! (released as atomic-json-store), so this module reproduces `json.loads`
//! (including its error messages and positions, `NaN`/`Infinity`,
//! arbitrary-size integers and duplicate-key handling) and `json.dumps` with
//! the `indent`, `sort_keys` and `ensure_ascii` settings the store uses. Values are ordinary
//! [`serde_json::Value`]s; numbers keep the canonical text CPython would print
//! for them, so integers of any size and non-finite floats survive a round
//! trip unchanged.
//!
//! Both the reader and the writer are iterative, so deeply nested documents
//! cannot overflow the stack.

use std::fmt;

use serde_json::{Map, Number, Value};

use crate::python::float_repr;

/// A decoding failure, formatted like CPython's `JSONDecodeError` /
/// `UnicodeDecodeError` messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    msg: String,
    /// Character (code point) offset of the failure, when the message carries one.
    pos: Option<usize>,
    lineno: usize,
    colno: usize,
}

impl JsonError {
    fn at(msg: &str, doc: &str, byte_pos: usize) -> Self {
        let before = &doc.as_bytes()[..byte_pos.min(doc.len())];
        let pos = before.iter().filter(|b| (**b & 0xc0) != 0x80).count();
        let lineno = before.iter().filter(|b| **b == b'\n').count() + 1;
        let colno = match before.iter().rposition(|b| *b == b'\n') {
            Some(nl) => {
                before[nl + 1..]
                    .iter()
                    .filter(|b| (**b & 0xc0) != 0x80)
                    .count()
                    + 1
            }
            None => pos + 1,
        };
        JsonError {
            msg: msg.to_owned(),
            pos: Some(pos),
            lineno,
            colno,
        }
    }

    fn plain(msg: String) -> Self {
        JsonError {
            msg,
            pos: None,
            lineno: 0,
            colno: 0,
        }
    }

    /// The bare message without the position suffix (e.g. `Expecting value`).
    pub fn msg(&self) -> &str {
        &self.msg
    }

    /// Character offset of the error, when known.
    pub fn pos(&self) -> Option<usize> {
        self.pos
    }

    /// 1-based line of the error (0 when unknown).
    pub fn lineno(&self) -> usize {
        self.lineno
    }

    /// 1-based column of the error (0 when unknown).
    pub fn colno(&self) -> usize {
        self.colno
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pos {
            Some(pos) => write!(
                f,
                "{}: line {} column {} (char {})",
                self.msg, self.lineno, self.colno, pos
            ),
            None => f.write_str(&self.msg),
        }
    }
}

impl std::error::Error for JsonError {}

/// Parse a JSON document from text, like `json.loads(str)`.
///
/// A leading byte-order mark is rejected, as CPython does for `str` input.
pub fn loads(s: &str) -> Result<Value, JsonError> {
    if s.starts_with('\u{feff}') {
        return Err(JsonError::at(
            "Unexpected UTF-8 BOM (decode using utf-8-sig)",
            s,
            0,
        ));
    }
    Parser::new(s).decode()
}

/// Parse a JSON document from raw bytes, like `json.loads(bytes)`.
///
/// The encoding is detected the way CPython does (UTF-8 with or without BOM,
/// UTF-16 and UTF-32 with or without BOM) and decoding errors carry CPython's
/// messages. CPython decodes with `surrogatepass`, so an encoded lone
/// surrogate is not a decoding error there; such input is reported with the
/// JSON error CPython would raise for it, or, when it sits inside a string
/// (which a Rust `String` cannot hold), as an unpaired surrogate.
pub fn loads_bytes(b: &[u8]) -> Result<Value, JsonError> {
    let (text, surrogate) = decode_bytes(b)?;
    let value = Parser::new(&text).decode()?;
    match surrogate {
        Some(pos) => Err(JsonError::at(
            "Unpaired surrogate in the encoded input",
            &text,
            pos,
        )),
        None => Ok(value),
    }
}

fn detect_encoding(b: &[u8]) -> &'static str {
    if b.starts_with(&[0, 0, 0xfe, 0xff]) || b.starts_with(&[0xff, 0xfe, 0, 0]) {
        return "utf-32";
    }
    if b.starts_with(&[0xfe, 0xff]) || b.starts_with(&[0xff, 0xfe]) {
        return "utf-16";
    }
    if b.starts_with(&[0xef, 0xbb, 0xbf]) {
        return "utf-8-sig";
    }
    if b.len() >= 4 {
        if b[0] == 0 {
            return if b[1] != 0 { "utf-16-be" } else { "utf-32-be" };
        }
        if b[1] == 0 {
            return if b[2] != 0 || b[3] != 0 {
                "utf-16-le"
            } else {
                "utf-32-le"
            };
        }
    } else if b.len() == 2 {
        if b[0] == 0 {
            return "utf-16-be";
        }
        if b[1] == 0 {
            return "utf-16-le";
        }
    }
    "utf-8"
}

/// Stand-in for a decoded lone surrogate. Like the surrogate, it is invalid
/// JSON outside a string, so syntax errors and their positions match CPython.
const SURROGATE_STANDIN: char = '\u{fffd}';

/// Decoded text plus the byte offset (in the text) of the first lone
/// surrogate, if any.
type Decoded = (String, Option<usize>);

fn decode_bytes(b: &[u8]) -> Result<Decoded, JsonError> {
    match detect_encoding(b) {
        "utf-8" => decode_utf8(b),
        // CPython's utf-8-sig decoder reports errors as 'utf-8', with
        // positions counted after the BOM.
        "utf-8-sig" => decode_utf8(&b[3..]),
        "utf-16" => decode_utf16(b, 2, b.starts_with(&[0xfe, 0xff])),
        "utf-16-be" => decode_utf16(b, 0, true),
        "utf-16-le" => decode_utf16(b, 0, false),
        "utf-32" => decode_utf32(b, 4, b.starts_with(&[0, 0, 0xfe, 0xff])),
        "utf-32-be" => decode_utf32(b, 0, true),
        _ => decode_utf32(b, 0, false),
    }
}

/// UTF-8 with CPython's `surrogatepass`: `ED A0..BF 80..BF` decodes to a lone
/// surrogate instead of failing.
fn decode_utf8(b: &[u8]) -> Result<Decoded, JsonError> {
    let mut out = String::with_capacity(b.len());
    let mut surrogate = None;
    let mut i = 0;
    while i < b.len() {
        match std::str::from_utf8(&b[i..]) {
            Ok(rest) => {
                out.push_str(rest);
                break;
            }
            Err(e) => {
                let bad = i + e.valid_up_to();
                out.push_str(std::str::from_utf8(&b[i..bad]).unwrap_or_default());
                if b.len() >= bad + 3
                    && b[bad] == 0xed
                    && (0xa0..=0xbf).contains(&b[bad + 1])
                    && (0x80..=0xbf).contains(&b[bad + 2])
                {
                    surrogate.get_or_insert(out.len());
                    out.push(SURROGATE_STANDIN);
                    i = bad + 3;
                } else {
                    return Err(utf8_error(b, bad, e.error_len()));
                }
            }
        }
    }
    Ok((out, surrogate))
}

fn utf8_error(b: &[u8], start: usize, error_len: Option<usize>) -> JsonError {
    let (end, reason) = match error_len {
        None => (b.len(), "unexpected end of data"),
        Some(n) => {
            let lead = b[start];
            if (0xc2..=0xf4).contains(&lead) {
                (start + n, "invalid continuation byte")
            } else {
                (start + n, "invalid start byte")
            }
        }
    };
    codec_error("utf-8", b, start, end, reason)
}

/// CPython's `UnicodeDecodeError.__str__`.
fn codec_error(codec: &str, b: &[u8], start: usize, end: usize, reason: &str) -> JsonError {
    if end - start <= 1 {
        JsonError::plain(format!(
            "'{codec}' codec can't decode byte 0x{:02x} in position {start}: {reason}",
            b[start]
        ))
    } else {
        JsonError::plain(format!(
            "'{codec}' codec can't decode bytes in position {start}-{}: {reason}",
            end - 1
        ))
    }
}

/// UTF-16 from byte `skip` on (after any BOM), with `surrogatepass`.
/// Positions in errors count from the start of the input, BOM included, and
/// the codec is named with its byte order, as CPython does.
fn decode_utf16(b: &[u8], skip: usize, big: bool) -> Result<Decoded, JsonError> {
    let codec = if big { "utf-16-be" } else { "utf-16-le" };
    let unit = |i: usize| {
        let pair = [b[i], b[i + 1]];
        if big {
            u16::from_be_bytes(pair)
        } else {
            u16::from_le_bytes(pair)
        }
    };
    let mut out = String::with_capacity(b.len() / 2);
    let mut surrogate = None;
    let mut i = skip;
    while i + 1 < b.len() {
        let u = unit(i);
        i += 2;
        if (0xd800..0xdc00).contains(&u) && i + 1 < b.len() {
            let low = unit(i);
            if (0xdc00..0xe000).contains(&low) {
                let c = 0x10000 + ((u32::from(u) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
                out.push(char::from_u32(c).unwrap_or(SURROGATE_STANDIN));
                i += 2;
                continue;
            }
        }
        match char::from_u32(u32::from(u)) {
            Some(c) => out.push(c),
            None => {
                surrogate.get_or_insert(out.len());
                out.push(SURROGATE_STANDIN);
            }
        }
    }
    if i < b.len() {
        return Err(codec_error(codec, b, i, i + 1, "truncated data"));
    }
    Ok((out, surrogate))
}

/// UTF-32 from byte `skip` on, with `surrogatepass` (see [`decode_utf16`]).
fn decode_utf32(b: &[u8], skip: usize, big: bool) -> Result<Decoded, JsonError> {
    let codec = if big { "utf-32-be" } else { "utf-32-le" };
    let mut out = String::with_capacity(b.len() / 4);
    let mut surrogate = None;
    let mut i = skip;
    while i + 3 < b.len() {
        let arr = [b[i], b[i + 1], b[i + 2], b[i + 3]];
        let v = if big {
            u32::from_be_bytes(arr)
        } else {
            u32::from_le_bytes(arr)
        };
        match char::from_u32(v) {
            Some(ch) => out.push(ch),
            None if (0xd800..0xe000).contains(&v) => {
                surrogate.get_or_insert(out.len());
                out.push(SURROGATE_STANDIN);
            }
            None => {
                return Err(codec_error(
                    codec,
                    b,
                    i,
                    i + 4,
                    "code point not in range(0x110000)",
                ));
            }
        }
        i += 4;
    }
    if i < b.len() {
        return Err(codec_error(codec, b, i, b.len(), "truncated data"));
    }
    Ok((out, surrogate))
}

/// Deepest container nesting accepted. The parser and serialiser are
/// iterative, but `serde_json::Value` is dropped (and cloned and compared)
/// recursively, so an unbounded depth lets a crafted file abort the process
/// with a stack overflow, even in the read-only `info()`. 4096 levels stay
/// safe on a 2 MiB thread stack, in debug builds too. CPython raised
/// `RecursionError` for such files too, at a depth that depends on the
/// version (about 1000 on 3.11, around 100 000 on 3.14).
pub const MAX_DEPTH: usize = 4_096;

enum Frame {
    Array(Vec<Value>),
    Object(Map<String, Value>, String),
}

struct Parser<'a> {
    s: &'a str,
    b: &'a [u8],
    /// First lone surrogate seen (byte offset of its `u`). CPython accepts lone
    /// surrogates but a Rust `String` cannot hold one, so the error is raised
    /// only if the document is otherwise valid.
    lone_surrogate: Option<usize>,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

fn hex_val(b: u8) -> Option<u32> {
    match b {
        b'0'..=b'9' => Some(u32::from(b - b'0')),
        b'a'..=b'f' => Some(u32::from(b - b'a' + 10)),
        b'A'..=b'F' => Some(u32::from(b - b'A' + 10)),
        _ => None,
    }
}

/// Build a number from canonical text. `arbitrary_precision` stores numbers as
/// text; the constructor is doc-hidden in serde_json but stable in the exactly
/// pinned version this crate depends on.
fn number(text: String) -> Value {
    Value::Number(Number::from_string_unchecked(text))
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Parser {
            s,
            b: s.as_bytes(),
            lone_surrogate: None,
        }
    }

    fn err(&self, msg: &str, pos: usize) -> JsonError {
        JsonError::at(msg, self.s, pos)
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while i < self.b.len() && is_ws(self.b[i]) {
            i += 1;
        }
        i
    }

    fn decode(mut self) -> Result<Value, JsonError> {
        let idx = self.skip_ws(0);
        let (value, end) = self.scan(idx)?;
        let end = self.skip_ws(end);
        if end != self.b.len() {
            return Err(self.err("Extra data", end));
        }
        if let Some(pos) = self.lone_surrogate {
            return Err(self.err("Unpaired surrogate in \\uXXXX escape", pos));
        }
        Ok(value)
    }

    fn starts_with_at(&self, idx: usize, lit: &[u8]) -> bool {
        self.b.len() >= idx + lit.len() && &self.b[idx..idx + lit.len()] == lit
    }

    fn open_container(&self, stack: &[Frame], idx: usize) -> Result<(), JsonError> {
        if stack.len() >= MAX_DEPTH {
            return Err(self.err(
                &format!("Nesting deeper than {MAX_DEPTH} levels is not supported"),
                idx,
            ));
        }
        Ok(())
    }

    /// Parse one value starting at `idx`; returns the value and the index
    /// just past it.
    fn scan(&mut self, mut idx: usize) -> Result<(Value, usize), JsonError> {
        let mut stack: Vec<Frame> = Vec::new();
        'value: loop {
            // Parse a value at idx (or open a container and continue).
            let (mut value, mut end) = match self.b.get(idx) {
                None => return Err(self.err("Expecting value", idx)),
                Some(b'"') => {
                    let (s, end) = self.scanstring(idx + 1)?;
                    (Value::String(s), end)
                }
                Some(b'{') => {
                    self.open_container(&stack, idx)?;
                    let i = self.skip_ws(idx + 1);
                    if self.b.get(i) == Some(&b'}') {
                        (Value::Object(Map::new()), i + 1)
                    } else {
                        let (key, next) = self.object_key(i)?;
                        stack.push(Frame::Object(Map::new(), key));
                        idx = next;
                        continue 'value;
                    }
                }
                Some(b'[') => {
                    self.open_container(&stack, idx)?;
                    let i = self.skip_ws(idx + 1);
                    if self.b.get(i) == Some(&b']') {
                        (Value::Array(Vec::new()), i + 1)
                    } else {
                        stack.push(Frame::Array(Vec::new()));
                        idx = i;
                        continue 'value;
                    }
                }
                Some(_) => match self.scan_scalar(idx) {
                    Some(found) => found,
                    None => return Err(self.err("Expecting value", idx)),
                },
            };
            // Hand the finished value to its container(s).
            loop {
                match stack.last_mut() {
                    None => return Ok((value, end)),
                    Some(Frame::Array(items)) => {
                        items.push(value);
                        let i = self.skip_ws(end);
                        if self.b.get(i) == Some(&b']') {
                            let Some(Frame::Array(items)) = stack.pop() else {
                                unreachable!()
                            };
                            value = Value::Array(items);
                            end = i + 1;
                            continue;
                        }
                        if self.b.get(i) != Some(&b',') {
                            return Err(self.err("Expecting ',' delimiter", i));
                        }
                        let next = self.skip_ws(i + 1);
                        if self.b.get(next) == Some(&b']') {
                            return Err(self.err("Illegal trailing comma before end of array", i));
                        }
                        idx = next;
                        continue 'value;
                    }
                    Some(Frame::Object(map, key)) => {
                        map.insert(std::mem::take(key), value);
                        let i = self.skip_ws(end);
                        if self.b.get(i) == Some(&b'}') {
                            let Some(Frame::Object(map, _)) = stack.pop() else {
                                unreachable!()
                            };
                            value = Value::Object(map);
                            end = i + 1;
                            continue;
                        }
                        if self.b.get(i) != Some(&b',') {
                            return Err(self.err("Expecting ',' delimiter", i));
                        }
                        let next = self.skip_ws(i + 1);
                        if self.b.get(next) == Some(&b'}') {
                            return Err(self.err("Illegal trailing comma before end of object", i));
                        }
                        let (new_key, after) = self.object_key(next)?;
                        if let Some(Frame::Object(_, key)) = stack.last_mut() {
                            *key = new_key;
                        }
                        idx = after;
                        continue 'value;
                    }
                }
            }
        }
    }

    /// Read `"key"` `:` at `idx`; returns the key and the index of the value.
    fn object_key(&mut self, idx: usize) -> Result<(String, usize), JsonError> {
        if self.b.get(idx) != Some(&b'"') {
            return Err(self.err("Expecting property name enclosed in double quotes", idx));
        }
        let (key, end) = self.scanstring(idx + 1)?;
        let i = self.skip_ws(end);
        if self.b.get(i) != Some(&b':') {
            return Err(self.err("Expecting ':' delimiter", i));
        }
        Ok((key, self.skip_ws(i + 1)))
    }

    fn scan_scalar(&self, idx: usize) -> Option<(Value, usize)> {
        match self.b[idx] {
            b'n' if self.starts_with_at(idx, b"null") => return Some((Value::Null, idx + 4)),
            b't' if self.starts_with_at(idx, b"true") => {
                return Some((Value::Bool(true), idx + 4));
            }
            b'f' if self.starts_with_at(idx, b"false") => {
                return Some((Value::Bool(false), idx + 5));
            }
            b'N' if self.starts_with_at(idx, b"NaN") => {
                return Some((number("NaN".to_owned()), idx + 3));
            }
            b'I' if self.starts_with_at(idx, b"Infinity") => {
                return Some((number("Infinity".to_owned()), idx + 8));
            }
            b'-' if self.starts_with_at(idx, b"-Infinity") => {
                return Some((number("-Infinity".to_owned()), idx + 9));
            }
            _ => {}
        }
        self.match_number(idx)
    }

    fn match_number(&self, start: usize) -> Option<(Value, usize)> {
        let b = self.b;
        let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
        let mut idx = start;
        if b[idx] == b'-' {
            idx += 1;
            if idx >= b.len() {
                return None;
            }
        }
        match b[idx] {
            b'1'..=b'9' => {
                idx += 1;
                while digit(idx) {
                    idx += 1;
                }
            }
            b'0' => idx += 1,
            _ => return None,
        }
        let mut is_float = false;
        if idx + 1 < b.len() && b[idx] == b'.' && digit(idx + 1) {
            is_float = true;
            idx += 2;
            while digit(idx) {
                idx += 1;
            }
        }
        if idx + 1 < b.len() && (b[idx] == b'e' || b[idx] == b'E') {
            let e_start = idx;
            idx += 1;
            if idx + 1 < b.len() && (b[idx] == b'-' || b[idx] == b'+') {
                idx += 1;
            }
            while digit(idx) {
                idx += 1;
            }
            if b[idx - 1].is_ascii_digit() {
                is_float = true;
            } else {
                idx = e_start;
            }
        }
        let text = &self.s[start..idx];
        let canonical = if is_float {
            let x: f64 = text.parse().unwrap_or(f64::NAN);
            float_json_text(x)
        } else if text == "-0" {
            "0".to_owned()
        } else {
            text.to_owned()
        };
        Some((number(canonical), idx))
    }

    /// Decode a string whose opening quote is at `end - 1`, mirroring
    /// CPython's `scanstring_unicode`.
    fn scanstring(&mut self, mut end: usize) -> Result<(String, usize), JsonError> {
        let b = self.b;
        let begin = end - 1;
        let mut out = String::new();
        loop {
            let mut next = end;
            let mut terminator = None;
            while next < b.len() {
                let d = b[next];
                if d == b'"' || d == b'\\' {
                    terminator = Some(d);
                    break;
                }
                if d <= 0x1f {
                    return Err(self.err("Invalid control character at", next));
                }
                next += 1;
            }
            let Some(c) = terminator else {
                return Err(self.err("Unterminated string starting at", begin));
            };
            out.push_str(&self.s[end..next]);
            next += 1;
            if c == b'"' {
                end = next;
                break;
            }
            if next == b.len() {
                return Err(self.err("Unterminated string starting at", begin));
            }
            let esc = b[next];
            if esc != b'u' {
                end = next + 1;
                let ch = match esc {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'/' => '/',
                    b'b' => '\u{8}',
                    b'f' => '\u{c}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    _ => return Err(self.err("Invalid \\escape", end - 2)),
                };
                out.push(ch);
                continue;
            }
            let u_pos = next;
            next += 1;
            let mut c = self.hex4(next, u_pos)?;
            end = next + 4;
            next = end;
            if (0xd800..=0xdbff).contains(&c)
                && self.chars_remaining_more_than(end, 6)
                && b[next] == b'\\'
                && b[next + 1] == b'u'
            {
                let c2 = self.hex4(next + 2, next + 1)?;
                if (0xdc00..=0xdfff).contains(&c2) {
                    c = 0x10000 + (((c - 0xd800) << 10) | (c2 - 0xdc00));
                    end += 6;
                }
            }
            match char::from_u32(c) {
                Some(ch) => out.push(ch),
                None => {
                    if self.lone_surrogate.is_none() {
                        self.lone_surrogate = Some(u_pos);
                    }
                    out.push('\u{fffd}');
                }
            }
        }
        Ok((out, end))
    }

    fn hex4(&self, start: usize, u_pos: usize) -> Result<u32, JsonError> {
        if start + 4 > self.b.len() {
            return Err(self.err("Invalid \\uXXXX escape", u_pos));
        }
        let mut c = 0u32;
        for i in start..start + 4 {
            match hex_val(self.b[i]) {
                Some(v) => c = (c << 4) | v,
                None => return Err(self.err("Invalid \\uXXXX escape", u_pos)),
            }
        }
        Ok(c)
    }

    fn chars_remaining_more_than(&self, from: usize, n: usize) -> bool {
        self.s
            .get(from..)
            .map(|rest| rest.chars().take(n + 1).count() > n)
            .unwrap_or(false)
    }
}

/// The JSON text CPython's encoder writes for a float (`NaN`, `Infinity`,
/// `-Infinity` for non-finite values, `repr()` otherwise).
pub fn float_json_text(x: f64) -> String {
    if x.is_nan() {
        "NaN".to_owned()
    } else if x == f64::INFINITY {
        "Infinity".to_owned()
    } else if x == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        float_repr(x)
    }
}

/// Serialisation settings mirroring the `json.dumps` keyword arguments used by
/// the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpOptions {
    /// Indentation string per level; `None` writes everything on one line with
    /// `", "` and `": "` separators.
    pub indent: Option<String>,
    /// Sort object keys (code point order).
    pub sort_keys: bool,
    /// Escape every non-ASCII character as `\uXXXX`.
    pub ensure_ascii: bool,
}

impl Default for DumpOptions {
    fn default() -> Self {
        DumpOptions {
            indent: Some("  ".to_owned()),
            sort_keys: false,
            ensure_ascii: false,
        }
    }
}

impl DumpOptions {
    /// `json.dumps(value, indent=n, ensure_ascii=False)`.
    pub fn indent(n: usize) -> Self {
        DumpOptions {
            indent: Some(" ".repeat(n)),
            ..DumpOptions::default()
        }
    }
}

fn write_number(out: &mut String, n: &Number) {
    let text = n.to_string();
    match text.as_str() {
        "NaN" | "Infinity" | "-Infinity" => out.push_str(&text),
        t if t.contains(['.', 'e', 'E']) => {
            let x: f64 = t.parse().unwrap_or(f64::NAN);
            out.push_str(&float_json_text(x));
        }
        "-0" => out.push('0'),
        t => out.push_str(t),
    }
}

fn write_string(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if !ensure_ascii || (' '..='~').contains(&c) => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
        }
    }
    out.push('"');
}

enum Level<'v> {
    Array(std::slice::Iter<'v, Value>),
    Object(std::vec::IntoIter<(&'v str, &'v Value)>),
}

/// Serialise a value like `json.dumps(value, indent=..., sort_keys=...,
/// ensure_ascii=...)` (no trailing newline).
pub fn dumps(value: &Value, opts: &DumpOptions) -> String {
    let mut out = String::new();
    dump_into(&mut out, Some(value), Vec::new(), opts);
    out
}

/// Serialise an object given as ordered `(key, value)` entries without first
/// building a `Map` (used for the store envelope, so the payload is never
/// cloned).
pub(crate) fn dumps_entries(entries: Vec<(&str, &Value)>, opts: &DumpOptions) -> String {
    let mut out = String::new();
    dump_into(&mut out, None, entries, opts);
    out
}

fn object_level<'v>(mut entries: Vec<(&'v str, &'v Value)>, opts: &DumpOptions) -> Level<'v> {
    if opts.sort_keys {
        entries.sort_by(|a, b| a.0.cmp(b.0));
    }
    Level::Object(entries.into_iter())
}

fn dump_into<'v>(
    out: &mut String,
    root: Option<&'v Value>,
    root_entries: Vec<(&'v str, &'v Value)>,
    opts: &DumpOptions,
) {
    let item_sep = if opts.indent.is_some() { "," } else { ", " };
    let newline = |out: &mut String, depth: usize| {
        if let Some(ind) = &opts.indent {
            out.push('\n');
            for _ in 0..depth {
                out.push_str(ind);
            }
        }
    };
    let mut stack: Vec<(Level<'v>, bool)> = Vec::new();
    let mut pending: Option<&'v Value> = root;
    if root.is_none() {
        if root_entries.is_empty() {
            out.push_str("{}");
            return;
        }
        out.push('{');
        stack.push((object_level(root_entries, opts), true));
    }
    loop {
        if let Some(v) = pending.take() {
            match v {
                Value::Null => out.push_str("null"),
                Value::Bool(true) => out.push_str("true"),
                Value::Bool(false) => out.push_str("false"),
                Value::Number(n) => write_number(out, n),
                Value::String(s) => write_string(out, s, opts.ensure_ascii),
                Value::Array(items) if items.is_empty() => out.push_str("[]"),
                Value::Object(map) if map.is_empty() => out.push_str("{}"),
                Value::Array(items) => {
                    out.push('[');
                    stack.push((Level::Array(items.iter()), true));
                }
                Value::Object(map) => {
                    out.push('{');
                    let entries = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
                    stack.push((object_level(entries, opts), true));
                }
            }
        }
        let depth = stack.len();
        let Some((level, first)) = stack.last_mut() else {
            break;
        };
        let next = match level {
            Level::Array(it) => it.next().map(|v| (None, v)),
            Level::Object(it) => it.next().map(|(k, v)| (Some(k), v)),
        };
        match next {
            Some((key, v)) => {
                if !*first {
                    out.push_str(item_sep);
                }
                *first = false;
                newline(out, depth);
                if let Some(k) = key {
                    write_string(out, k, opts.ensure_ascii);
                    out.push_str(": ");
                }
                pending = Some(v);
            }
            None => {
                let closing = match level {
                    Level::Array(_) => ']',
                    Level::Object(_) => '}',
                };
                stack.pop();
                newline(out, depth - 1);
                out.push(closing);
            }
        }
    }
}

/// Whether `value` nests containers more than `limit` levels deep (checked
/// iteratively).
pub(crate) fn nests_deeper_than(value: &Value, limit: usize) -> bool {
    let mut stack: Vec<(&Value, usize)> = vec![(value, 1)];
    while let Some((v, depth)) = stack.pop() {
        let is_container = matches!(v, Value::Array(_) | Value::Object(_));
        if is_container && depth > limit {
            return true;
        }
        match v {
            Value::Array(items) => stack.extend(items.iter().map(|c| (c, depth + 1))),
            Value::Object(map) => stack.extend(map.values().map(|c| (c, depth + 1))),
            _ => {}
        }
    }
    false
}

/// Rewrite numbers into the text form serde's typed deserialisers expect
/// (CPython spells `1e-05` where serde_json expects `1e-5`). Non-finite
/// floats become `null`, the only JSON-compatible representation.
pub(crate) fn normalize_numbers_for_serde(value: &mut Value) {
    let mut stack: Vec<&mut Value> = vec![value];
    while let Some(v) = stack.pop() {
        match v {
            Value::Number(n) => {
                let text = n.to_string();
                if text.contains(['.', 'e', 'E']) || text.ends_with("Infinity") || text == "NaN" {
                    let x: f64 = text.parse().unwrap_or(f64::NAN);
                    *v = Number::from_f64(x).map_or(Value::Null, Value::Number);
                }
            }
            Value::Array(items) => stack.extend(items.iter_mut()),
            Value::Object(map) => stack.extend(map.values_mut()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(s: &str) -> String {
        loads(s).unwrap_err().to_string()
    }

    #[test]
    fn decode_errors_match_cpython() {
        let cases = [
            ("", "Expecting value: line 1 column 1 (char 0)"),
            (" ", "Expecting value: line 1 column 2 (char 1)"),
            (
                "{",
                "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
            ),
            (
                "{\"a\"",
                "Expecting ':' delimiter: line 1 column 5 (char 4)",
            ),
            ("{\"a\":", "Expecting value: line 1 column 6 (char 5)"),
            (
                "{\"a\":1",
                "Expecting ',' delimiter: line 1 column 7 (char 6)",
            ),
            (
                "{\"a\":1,}",
                "Illegal trailing comma before end of object: line 1 column 7 (char 6)",
            ),
            (
                "[1,]",
                "Illegal trailing comma before end of array: line 1 column 3 (char 2)",
            ),
            ("[", "Expecting value: line 1 column 2 (char 1)"),
            ("[1 2]", "Expecting ',' delimiter: line 1 column 4 (char 3)"),
            (
                "\"abc",
                "Unterminated string starting at: line 1 column 1 (char 0)",
            ),
            ("\"a\\x\"", "Invalid \\escape: line 1 column 3 (char 2)"),
            (
                "\"a\\u12\"",
                "Invalid \\uXXXX escape: line 1 column 4 (char 3)",
            ),
            (
                "\"\x01\"",
                "Invalid control character at: line 1 column 2 (char 1)",
            ),
            ("-", "Expecting value: line 1 column 1 (char 0)"),
            ("01", "Extra data: line 1 column 2 (char 1)"),
            ("1.", "Extra data: line 1 column 2 (char 1)"),
            ("1e+", "Extra data: line 1 column 2 (char 1)"),
            ("-NaN", "Expecting value: line 1 column 1 (char 0)"),
            (
                "\u{feff}{}",
                "Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)",
            ),
            (
                "[1,\n2,\n]",
                "Illegal trailing comma before end of array: line 2 column 2 (char 5)",
            ),
            (
                "\"\\u0041",
                "Unterminated string starting at: line 1 column 1 (char 0)",
            ),
            (
                "\"\\ud834\\uzzzz\"",
                "Invalid \\uXXXX escape: line 1 column 9 (char 8)",
            ),
            (
                "\"\\ud834\\x\"",
                "Invalid \\escape: line 1 column 8 (char 7)",
            ),
            ("[\"é\", x]", "Expecting value: line 1 column 7 (char 6)"),
        ];
        for (input, want) in cases {
            assert_eq!(err(input), want, "input {input:?}");
        }
    }

    #[test]
    fn values_round_trip_like_cpython() {
        let compact = DumpOptions {
            indent: None,
            ..DumpOptions::default()
        };
        let cases = [
            ("NaN", "NaN"),
            ("-Infinity", "-Infinity"),
            ("1e400", "Infinity"),
            ("-0", "0"),
            ("-0.0", "-0.0"),
            ("1E2", "100.0"),
            (
                "123456789012345678901234567890",
                "123456789012345678901234567890",
            ),
            ("\"\\ud834\\udd1e\"", "\"𝄞\""),
            ("{\"a\":1,\"a\":2}", "{\"a\": 2}"),
            ("{\"b\":1,\"a\":2,\"b\":3}", "{\"b\": 3, \"a\": 2}"),
            ("\"\\u007f\\u001f\"", "\"\u{7f}\\u001f\""),
            ("[1e16, 1e15, 1e-5]", "[1e+16, 1000000000000000.0, 1e-05]"),
        ];
        for (input, want) in cases {
            assert_eq!(dumps(&loads(input).unwrap(), &compact), want, "{input}");
        }
    }

    #[test]
    fn indented_output_matches_cpython() {
        let v = loads(r#"{"a":[],"b":{},"c":[1,[2,{}]],"d":"é"}"#).unwrap();
        assert_eq!(
            dumps(&v, &DumpOptions::default()),
            "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    1,\n    [\n      2,\n      {}\n    ]\n  ],\n  \"d\": \"é\"\n}"
        );
        let ascii = DumpOptions {
            indent: None,
            sort_keys: true,
            ensure_ascii: true,
        };
        let v = loads(r#"{"z":"é\u2028𝄞","a":1}"#).unwrap();
        assert_eq!(
            dumps(&v, &ascii),
            r#"{"a": 1, "z": "\u00e9\u2028\ud834\udd1e"}"#
        );
        let zero = DumpOptions::indent(0);
        assert_eq!(
            dumps(&loads(r#"{"a":[1,2]}"#).unwrap(), &zero),
            "{\n\"a\": [\n1,\n2\n]\n}"
        );
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let depth = MAX_DEPTH;
        let doc = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let v = loads(&doc).unwrap();
        let compact = DumpOptions {
            indent: None,
            ..DumpOptions::default()
        };
        assert_eq!(dumps(&v, &compact), doc);
        // Dismantle iteratively so the test itself does not recurse on drop.
        let mut cur = v;
        while let Value::Array(mut items) = cur {
            cur = items.pop().unwrap_or(Value::Null);
        }
    }

    #[test]
    fn lone_surrogates_are_rejected_after_syntax_checks() {
        assert_eq!(
            err("\"\\ud800\""),
            "Unpaired surrogate in \\uXXXX escape: line 1 column 3 (char 2)"
        );
        assert_eq!(
            err("[\"\\ud800\", x]"),
            "Expecting value: line 1 column 12 (char 11)"
        );
    }

    #[test]
    fn bytes_detect_encoding() {
        assert_eq!(
            loads_bytes(b"\xef\xbb\xbf{}").unwrap(),
            Value::Object(Map::new())
        );
        let utf16: Vec<u8> = "[1]".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(
            dumps(&loads_bytes(&utf16).unwrap(), &DumpOptions::indent(0)),
            "[\n1\n]"
        );
        assert!(
            loads_bytes(b"\xff")
                .unwrap_err()
                .to_string()
                .contains("invalid start byte")
        );
    }

    #[test]
    fn nesting_depth_is_bounded() {
        let ok = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(loads(&ok).is_ok());
        let deep = "[".repeat(MAX_DEPTH + 1) + &"]".repeat(MAX_DEPTH + 1);
        // Never Debug-print a deep Value in an assertion: that recurses too.
        assert_eq!(
            loads(&deep).map(|_| ()).unwrap_err().to_string(),
            format!(
                "Nesting deeper than {MAX_DEPTH} levels is not supported: line 1 column {} (char {})",
                MAX_DEPTH + 1,
                MAX_DEPTH
            )
        );
        let objects = "{\"a\":".repeat(MAX_DEPTH + 1) + "1" + &"}".repeat(MAX_DEPTH + 1);
        assert!(loads(&objects).is_err());
        let objects_ok = "{\"a\":".repeat(MAX_DEPTH) + "1" + &"}".repeat(MAX_DEPTH);
        assert!(loads(&objects_ok).is_ok());
        // A million levels used to overflow the stack when the value dropped.
        let huge = "[".repeat(1_000_000);
        assert!(loads_bytes(huge.as_bytes()).is_err());
    }

    #[test]
    fn byte_decoding_errors_match_cpython_surrogatepass() {
        let err = |b: &[u8]| loads_bytes(b).unwrap_err().to_string();
        // Expected strings are CPython 3.14 `str(exc)` for `json.loads(b)`.
        let cases: [(&[u8], &str); 10] = [
            (
                b"\xef\xbb\xbf  {\x99",
                "'utf-8' codec can't decode byte 0x99 in position 3: invalid start byte",
            ),
            (
                b"\xff\xfe\x00\x00[\x00\x00\x00\x00\x00\x11\x00",
                "'utf-32-le' codec can't decode bytes in position 8-11: code point not in range(0x110000)",
            ),
            (
                b"\xff\xfe\x00\x00[\x00\x00\x00]\x00\x00",
                "'utf-32-le' codec can't decode bytes in position 8-10: truncated data",
            ),
            (
                b"\xff\xfe\x00\x00A\x00\x00\x00\x00\xd8\x00\x00A",
                "'utf-32-le' codec can't decode byte 0x41 in position 12: truncated data",
            ),
            (
                b"\xff\xfe[\x00]",
                "'utf-16-le' codec can't decode byte 0x5d in position 4: truncated data",
            ),
            (
                b"\xfe\xff\x00[\x00]\x00",
                "'utf-16-be' codec can't decode byte 0x00 in position 6: truncated data",
            ),
            // Encoded lone surrogates decode (surrogatepass); outside a string
            // they are JSON syntax errors at the same position as in CPython.
            (
                b"\xff\xfe[\x00\x00\xd8]\x00",
                "Expecting value: line 1 column 2 (char 1)",
            ),
            (b"1\xed\xa0\x80", "Extra data: line 1 column 2 (char 1)"),
            (
                b"\x00\x00\x001\x00\x00\xd8\x00",
                "Extra data: line 1 column 2 (char 1)",
            ),
            (
                b"\"\xed\xa0",
                "'utf-8' codec can't decode byte 0xed in position 1: invalid continuation byte",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(err(input), want, "{input:?}");
        }
        // Inside a string CPython keeps the surrogate; a Rust String cannot.
        assert!(err(b"\xff\xfe\"\x00\x00\xd8\"\x00").starts_with("Unpaired surrogate"));
        assert!(err(b"\"\xed\xa0\x80\"").starts_with("Unpaired surrogate"));
        // A valid UTF-16 surrogate pair is one character.
        assert_eq!(
            loads_bytes(b"\xff\xfe\"\x00\x00\xd8\x00\xdc\"\x00").unwrap(),
            Value::String("\u{10000}".to_owned())
        );
    }
}
