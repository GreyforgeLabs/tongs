//! Python-compatible formatting and parsing helpers.
//!
//! Before 2.0, tongs was the Python package atomic-json-store (1.x). These
//! helpers reproduce the few pieces of CPython behaviour that leak into the
//! on-disk format and the CLI output (`float.__repr__`, `str.__repr__`,
//! `pathlib.PurePosixPath` normalisation, `float()` / `int()` parsing) so that
//! files and messages stay byte-for-byte compatible with the Python
//! implementation.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// Format a float exactly like CPython's `repr(float)`.
///
/// Uses the shortest round-tripping digits and switches to exponent notation
/// when the decimal exponent is below -4 or above 16, as CPython does.
/// Non-finite values render as `nan`, `inf` and `-inf`.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_owned();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.to_owned();
    }
    // zmij (Ryu-style) yields the shortest round-tripping digits and breaks
    // ties to even, exactly like CPython's dtoa; Rust's `{:e}` rounds ties up
    // and so differs from `repr` on values such as 756290343670631.25.
    let mut buf = zmij::Buffer::new();
    let text = buf.format_finite(x.abs());
    let (mantissa, exp) = text.split_once(['e', 'E']).unwrap_or((text, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let int_len = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let all: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let trimmed_lead = all.trim_start_matches('0');
    let lead_zeros = (all.len() - trimmed_lead.len()) as i32;
    let mut digits = trimmed_lead.trim_end_matches('0').to_owned();
    let mut decpt = int_len + exp - lead_zeros;
    if digits.is_empty() {
        digits.push('0');
        decpt = 1;
    }
    let mut out = String::with_capacity(digits.len() + 8);
    if x.is_sign_negative() {
        out.push('-');
    }
    if decpt <= -4 || decpt > 16 {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        out.push('e');
        out.push(if e < 0 { '-' } else { '+' });
        let mag = e.unsigned_abs();
        if mag < 10 {
            out.push('0');
        }
        out.push_str(&mag.to_string());
    } else if decpt <= 0 {
        out.push_str("0.");
        for _ in 0..(-decpt) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let decpt = decpt as usize;
        if decpt >= digits.len() {
            out.push_str(&digits);
            for _ in 0..(decpt - digits.len()) {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..decpt]);
            out.push('.');
            out.push_str(&digits[decpt..]);
        }
    }
    out
}

fn is_printable(c: char) -> bool {
    // Approximation of CPython's str.isprintable(): everything except control
    // characters, separators other than ASCII space, format characters,
    // surrogates, private use and non-characters.
    let u = c as u32;
    !matches!(u,
        0x00..=0x1f
        | 0x7f..=0xa0
        | 0xad
        | 0x600..=0x605
        | 0x61c
        | 0x6dd
        | 0x70f
        | 0x890..=0x891
        | 0x8e2
        | 0x1680
        | 0x180e
        | 0x2000..=0x200f
        | 0x2028..=0x202f
        | 0x205f..=0x2064
        | 0x2066..=0x206f
        | 0x3000
        | 0xd800..=0xf8ff
        | 0xfdd0..=0xfdef
        | 0xfeff
        | 0xfff9..=0xfffb
        | 0xfffe..=0xffff
        | 0x110bd
        | 0x110cd
        | 0x13430..=0x1343f
        | 0x1bca0..=0x1bca3
        | 0x1d173..=0x1d17a
        | 0xe0001
        | 0xe0020..=0xe007f
        | 0xf0000..=0x10ffff
    ) && (u & 0xfffe) != 0xfffe
}

/// Format a string exactly like CPython's `repr(str)`.
pub fn str_repr(s: &str) -> String {
    repr_units(&s.chars().map(Unit::Char).collect::<Vec<_>>())
}

/// One element of a string as CPython sees an `os.fsdecode`d name: a real
/// character, or a byte that was not valid UTF-8 and became a lone
/// surrogate (`U+DC80..U+DCFF`, the `surrogateescape` error handler).
#[derive(Clone, Copy)]
enum Unit {
    Char(char),
    Escaped(u8),
}

fn os_units(s: &OsStr) -> Vec<Unit> {
    let mut out = Vec::new();
    for chunk in s.as_bytes().utf8_chunks() {
        out.extend(chunk.valid().chars().map(Unit::Char));
        out.extend(chunk.invalid().iter().map(|b| Unit::Escaped(*b)));
    }
    out
}

fn repr_units(units: &[Unit]) -> String {
    let has = |q: char| units.iter().any(|u| matches!(u, Unit::Char(c) if *c == q));
    let quote = if has('\'') && !has('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(units.len() + 2);
    out.push(quote);
    for unit in units {
        let c = match unit {
            Unit::Char(c) => *c,
            Unit::Escaped(b) => {
                out.push_str(&format!("\\udc{b:02x}"));
                continue;
            }
        };
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if (c as u32) < 0x7f => out.push(c),
            c if is_printable(c) => out.push(c),
            c if (c as u32) <= 0xff => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// `repr()` of an OS string (a file name or argument) as CPython prints it
/// after `os.fsdecode`: bytes that are not valid UTF-8 appear as `\udcXX`.
pub fn os_str_repr(s: &OsStr) -> String {
    repr_units(&os_units(s))
}

/// An OS string as CPython writes it to `stderr` (UTF-8 with the
/// `backslashreplace` handler): bytes that are not valid UTF-8 appear as the
/// text `\udcXX`, everything else verbatim.
pub fn os_str_display(s: &OsStr) -> String {
    let mut out = String::with_capacity(s.len());
    for unit in os_units(s) {
        match unit {
            Unit::Char(c) => out.push(c),
            Unit::Escaped(b) => out.push_str(&format!("\\udc{b:02x}")),
        }
    }
    out
}

/// `repr()` of a path as CPython would print it inside an `OSError` message.
pub fn path_repr(path: &Path) -> String {
    os_str_repr(path.as_os_str())
}

/// A path as CPython would print it in an f-string message on `stderr`.
pub fn path_display(path: &Path) -> String {
    os_str_display(path.as_os_str())
}

/// Normalise a path the way `pathlib.PurePosixPath` does: collapse repeated
/// separators, drop `.` components and trailing separators, keep exactly two
/// leading slashes, and turn the empty path into `.`.
pub fn normalize_path(path: &OsStr) -> PathBuf {
    let bytes = path.as_bytes();
    let root: &[u8] = if bytes.starts_with(b"//") && !bytes.starts_with(b"///") {
        b"//"
    } else if bytes.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let mut out: Vec<u8> = root.to_vec();
    let mut first = true;
    for part in bytes.split(|b| *b == b'/') {
        if part.is_empty() || part == b"." {
            continue;
        }
        if !first {
            out.push(b'/');
        }
        out.extend_from_slice(part);
        first = false;
    }
    if out.is_empty() {
        out.push(b'.');
    }
    PathBuf::from(OsString::from_vec(out))
}

/// The final component of a normalised path, as `PurePath.name` reports it
/// (empty for `.` and for the filesystem root).
pub fn path_name(path: &Path) -> &OsStr {
    let bytes = path.as_os_str().as_bytes();
    if bytes == b"." || bytes.iter().all(|b| *b == b'/') {
        return OsStr::new("");
    }
    match bytes.iter().rposition(|b| *b == b'/') {
        Some(i) => OsStr::from_bytes(&bytes[i + 1..]),
        None => path.as_os_str(),
    }
}

/// The logical parent of a normalised path, as `PurePath.parent` reports it.
pub fn path_parent(path: &Path) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    if path_name(path).is_empty() {
        return path.to_path_buf();
    }
    match bytes.iter().rposition(|b| *b == b'/') {
        None => PathBuf::from("."),
        Some(i) => {
            let head = &bytes[..i];
            if head.is_empty() {
                PathBuf::from("/")
            } else if head == b"/" {
                PathBuf::from("//")
            } else {
                PathBuf::from(OsStr::from_bytes(head))
            }
        }
    }
}

fn strip_python_whitespace(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// First code point (value 0) of every run of ten Unicode decimal digits
/// (general category Nd) outside ASCII, as of Unicode 16.0 (CPython 3.14).
const DECIMAL_ZEROS: [u32; 75] = [
    0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66, 0xde6,
    0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90, 0x1b50,
    0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
    0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50, 0x11d50,
    0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce, 0x1d7d8,
    0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
];

/// CPython's `_PyUnicode_TransformDecimalAndSpaceToASCII`, which `int()` and
/// `float()` apply first: Unicode whitespace becomes a space and Unicode
/// decimal digits (`'\u0663'`, `'\uff11'`) become ASCII digits. Any other
/// non-ASCII character makes the text unparseable (`None`).
fn decimal_and_space_to_ascii(s: &str) -> Option<String> {
    s.chars()
        .map(|c| {
            if c.is_ascii() {
                return Some(c);
            }
            if c.is_whitespace() {
                return Some(' ');
            }
            let u = c as u32;
            let i = DECIMAL_ZEROS.partition_point(|&z| z <= u);
            let zero = *DECIMAL_ZEROS.get(i.checked_sub(1)?)?;
            (u - zero < 10).then(|| char::from(b'0' + (u - zero) as u8))
        })
        .collect()
}

/// Remove PEP 515 underscores (`1_000`) when they only separate digits.
fn strip_digit_underscores(s: &str) -> Option<String> {
    if !s.contains('_') {
        return Some(s.to_owned());
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' {
            let before = i.checked_sub(1).map(|j| chars[j]);
            let after = chars.get(i + 1).copied();
            if !matches!(before, Some(b) if b.is_ascii_digit())
                || !matches!(after, Some(a) if a.is_ascii_digit())
            {
                return None;
            }
        } else {
            out.push(*c);
        }
    }
    Some(out)
}

/// Parse a string the way Python's `float()` does (surrounding whitespace,
/// digit underscores, Unicode decimal digits, `inf`/`infinity`/`nan` in any
/// case).
pub fn parse_float(s: &str) -> Option<f64> {
    let ascii = decimal_and_space_to_ascii(s)?;
    let t = strip_python_whitespace(&ascii);
    let lower = t.to_ascii_lowercase();
    let unsigned = lower.trim_start_matches(['+', '-']);
    if lower.len() - unsigned.len() > 1 {
        return None;
    }
    if matches!(unsigned, "inf" | "infinity" | "nan") {
        let neg = lower.starts_with('-');
        return Some(match (unsigned, neg) {
            ("nan", false) => f64::NAN,
            ("nan", true) => -f64::NAN,
            (_, false) => f64::INFINITY,
            (_, true) => f64::NEG_INFINITY,
        });
    }
    let cleaned = strip_digit_underscores(t)?;
    let body = cleaned.trim_start_matches(['+', '-']);
    if body.is_empty()
        || !body
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
    {
        return None;
    }
    cleaned.parse::<f64>().ok()
}

/// Parse a string the way Python's `int()` does (base 10, surrounding
/// whitespace, optional sign, digit underscores, Unicode decimal digits).
/// Values outside `i128` saturate to `i128::MAX` / `-i128::MAX`.
pub fn parse_int(s: &str) -> Option<i128> {
    let ascii = decimal_and_space_to_ascii(s)?;
    let t = strip_python_whitespace(&ascii);
    let (neg, body) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if body.is_empty() || !body.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let cleaned = strip_digit_underscores(body)?;
    if !cleaned.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Python ints are unbounded; saturate so callers see an out-of-range
    // value (an index past the end, a version that is too large) rather
    // than a parse failure.
    let magnitude: i128 = cleaned.parse().unwrap_or(i128::MAX);
    Some(if neg { -magnitude } else { magnitude })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_cpython() {
        let cases = [
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (1e-5, "1e-05"),
            (1e-4, "0.0001"),
            (123_456_789.123_456_79, "123456789.12345679"),
            (1.5e300, "1.5e+300"),
            (-0.0, "-0.0"),
            (0.0, "0.0"),
            (2.5e-7, "2.5e-07"),
            (1e22, "1e+22"),
            (12345678901234567890.0, "1.2345678901234567e+19"),
            (5e-324, "5e-324"),
            (10.0, "10.0"),
            (0.2, "0.2"),
            (100.0, "100.0"),
            (f64::MAX, "1.7976931348623157e+308"),
            (756290343670631.2, "756290343670631.2"),
            (-1799994771824.7812, "-1799994771824.7812"),
            (0.001, "0.001"),
            (123.0, "123.0"),
            (1.5e-7, "1.5e-07"),
        ];
        for (x, want) in cases {
            assert_eq!(float_repr(x), want, "{x:e}");
        }
    }

    #[test]
    fn str_repr_matches_cpython() {
        assert_eq!(str_repr("a..b"), "'a..b'");
        assert_eq!(str_repr("it's"), "\"it's\"");
        assert_eq!(str_repr("both'\""), "'both\\'\"'");
        assert_eq!(str_repr("tab\tnl\n\x01\x7f"), "'tab\\tnl\\n\\x01\\x7f'");
        assert_eq!(str_repr("Zürich ✓"), "'Zürich ✓'");
        assert_eq!(str_repr("\u{a0}\u{2028}"), "'\\xa0\\u2028'");
        let raw = OsStr::from_bytes(b"it's \xff\xc3.json");
        assert_eq!(os_str_repr(raw), "\"it's \\udcff\\udcc3.json\"");
        assert_eq!(os_str_display(raw), "it's \\udcff\\udcc3.json");
        assert_eq!(os_str_display(OsStr::new("Zürich")), "Zürich");
    }

    #[test]
    fn normalize_like_pathlib() {
        let n = |s: &str| normalize_path(OsStr::new(s)).to_string_lossy().into_owned();
        assert_eq!(n(""), ".");
        assert_eq!(n("./state.json"), "state.json");
        assert_eq!(n("a//b/./c/"), "a/b/c");
        assert_eq!(n("//x"), "//x");
        assert_eq!(n("///x"), "/x");
        assert_eq!(n("/"), "/");
        assert_eq!(n("../x"), "../x");
        assert_eq!(path_name(Path::new(".")), "");
        assert_eq!(path_name(Path::new("/")), "");
        assert_eq!(path_name(Path::new("a/b.json")), "b.json");
        assert_eq!(path_parent(Path::new("b.json")), Path::new("."));
        assert_eq!(path_parent(Path::new("/b.json")), Path::new("/"));
        assert_eq!(path_parent(Path::new("a/b/c")), Path::new("a/b"));
    }

    #[test]
    fn python_number_parsing() {
        assert_eq!(parse_float(" 1_0.5 "), Some(10.5));
        assert_eq!(parse_float("1__0"), None);
        assert_eq!(parse_float("abc"), None);
        assert!(parse_float("-Infinity").unwrap().is_infinite());
        assert!(parse_float("nan").unwrap().is_nan());
        assert_eq!(parse_float(".5"), Some(0.5));
        assert_eq!(parse_float("5."), Some(5.0));
        assert_eq!(parse_int(" +1_0 "), Some(10));
        assert_eq!(parse_int("-3"), Some(-3));
        assert_eq!(parse_int("1.5"), None);
        assert_eq!(parse_int("_1"), None);
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_int("\u{663}"), Some(3));
        assert_eq!(parse_int("9".repeat(60).as_str()), Some(i128::MAX));
        assert_eq!(parse_int(&format!("-{}", "9".repeat(60))), Some(-i128::MAX));
        assert_eq!(parse_int("\u{ff11}\u{ff10}"), Some(10));
        assert_eq!(parse_int("\u{3000}-\u{1d7d9}\u{a0}"), Some(-1));
        assert_eq!(parse_int("\u{b2}"), None);
        assert_eq!(parse_int("1\u{e9}"), None);
        assert_eq!(parse_float("\u{ff11}.\u{ff15}"), Some(1.5));
        assert_eq!(parse_float("\u{2003}2e\u{661}"), Some(20.0));
    }
}
