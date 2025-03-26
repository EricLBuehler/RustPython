use crate::atomic::{PyAtomic, Radium};
use crate::format::CharLen;
use crate::wtf8::{CodePoint, Wtf8, Wtf8Buf};
use ascii::{AsciiChar, AsciiStr, AsciiString};
use core::fmt;
use core::sync::atomic::Ordering::Relaxed;
use std::ops::{Bound, RangeBounds};

#[cfg(not(target_arch = "wasm32"))]
#[allow(non_camel_case_types)]
pub type wchar_t = libc::wchar_t;
#[cfg(target_arch = "wasm32")]
#[allow(non_camel_case_types)]
pub type wchar_t = u32;

/// Utf8 + state.ascii (+ PyUnicode_Kind in future)
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum StrKind {
    Ascii,
    Utf8,
    Wtf8,
}

impl std::ops::BitOr for StrKind {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        use StrKind::*;
        match (self, other) {
            (Wtf8, _) | (_, Wtf8) => Wtf8,
            (Utf8, _) | (_, Utf8) => Utf8,
            (Ascii, Ascii) => Ascii,
        }
    }
}

impl StrKind {
    pub fn is_ascii(&self) -> bool {
        matches!(self, Self::Ascii)
    }

    pub fn is_utf8(&self) -> bool {
        matches!(self, Self::Ascii | Self::Utf8)
    }

    #[inline(always)]
    pub fn can_encode(&self, code: CodePoint) -> bool {
        match self {
            StrKind::Ascii => code.is_ascii(),
            StrKind::Utf8 => code.to_char().is_some(),
            StrKind::Wtf8 => true,
        }
    }
}

pub trait DeduceStrKind {
    fn str_kind(&self) -> StrKind;
}

impl DeduceStrKind for str {
    fn str_kind(&self) -> StrKind {
        if self.is_ascii() {
            StrKind::Ascii
        } else {
            StrKind::Utf8
        }
    }
}

impl DeduceStrKind for Wtf8 {
    fn str_kind(&self) -> StrKind {
        if self.is_ascii() {
            StrKind::Ascii
        } else if self.is_utf8() {
            StrKind::Utf8
        } else {
            StrKind::Wtf8
        }
    }
}

impl DeduceStrKind for String {
    fn str_kind(&self) -> StrKind {
        (**self).str_kind()
    }
}

impl DeduceStrKind for Wtf8Buf {
    fn str_kind(&self) -> StrKind {
        (**self).str_kind()
    }
}

impl<T: DeduceStrKind + ?Sized> DeduceStrKind for &T {
    fn str_kind(&self) -> StrKind {
        (**self).str_kind()
    }
}

impl<T: DeduceStrKind + ?Sized> DeduceStrKind for Box<T> {
    fn str_kind(&self) -> StrKind {
        (**self).str_kind()
    }
}

#[derive(Debug)]
pub enum PyKindStr<'a> {
    Ascii(&'a AsciiStr),
    Utf8(&'a str),
    Wtf8(&'a Wtf8),
}

#[derive(Debug, Clone)]
pub struct StrData {
    data: Box<Wtf8>,
    kind: StrKind,
    len: StrLen,
}

struct StrLen(PyAtomic<usize>);

impl From<usize> for StrLen {
    #[inline(always)]
    fn from(value: usize) -> Self {
        Self(Radium::new(value))
    }
}

impl fmt::Debug for StrLen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.0.load(Relaxed);
        if len == usize::MAX {
            f.write_str("<uncomputed>")
        } else {
            len.fmt(f)
        }
    }
}

impl StrLen {
    #[inline(always)]
    fn zero() -> Self {
        0usize.into()
    }
    #[inline(always)]
    fn uncomputed() -> Self {
        usize::MAX.into()
    }
}

impl Clone for StrLen {
    fn clone(&self) -> Self {
        Self(self.0.load(Relaxed).into())
    }
}

impl Default for StrData {
    fn default() -> Self {
        Self {
            data: <Box<Wtf8>>::default(),
            kind: StrKind::Ascii,
            len: StrLen::zero(),
        }
    }
}

impl From<Box<Wtf8>> for StrData {
    fn from(value: Box<Wtf8>) -> Self {
        // doing the check is ~10x faster for ascii, and is actually only 2% slower worst case for
        // non-ascii; see https://github.com/RustPython/RustPython/pull/2586#issuecomment-844611532
        let kind = value.str_kind();
        unsafe { Self::new_str_unchecked(value, kind) }
    }
}

impl From<Box<str>> for StrData {
    #[inline]
    fn from(value: Box<str>) -> Self {
        // doing the check is ~10x faster for ascii, and is actually only 2% slower worst case for
        // non-ascii; see https://github.com/RustPython/RustPython/pull/2586#issuecomment-844611532
        let kind = value.str_kind();
        unsafe { Self::new_str_unchecked(value.into(), kind) }
    }
}

impl From<Box<AsciiStr>> for StrData {
    #[inline]
    fn from(value: Box<AsciiStr>) -> Self {
        Self {
            len: value.len().into(),
            data: value.into(),
            kind: StrKind::Ascii,
        }
    }
}

impl From<AsciiChar> for StrData {
    fn from(ch: AsciiChar) -> Self {
        AsciiString::from(ch).into_boxed_ascii_str().into()
    }
}

impl From<char> for StrData {
    fn from(ch: char) -> Self {
        if let Ok(ch) = ascii::AsciiChar::from_ascii(ch) {
            ch.into()
        } else {
            Self {
                data: ch.to_string().into(),
                kind: StrKind::Utf8,
                len: 1.into(),
            }
        }
    }
}

impl From<CodePoint> for StrData {
    fn from(ch: CodePoint) -> Self {
        if let Some(ch) = ch.to_char() {
            ch.into()
        } else {
            Self {
                data: Wtf8Buf::from(ch).into(),
                kind: StrKind::Wtf8,
                len: 1.into(),
            }
        }
    }
}

impl StrData {
    /// # Safety
    ///
    /// Given `bytes` must be valid data for given `kind`
    pub unsafe fn new_str_unchecked(data: Box<Wtf8>, kind: StrKind) -> Self {
        let len = match kind {
            StrKind::Ascii => data.len().into(),
            _ => StrLen::uncomputed(),
        };
        Self { data, kind, len }
    }

    /// # Safety
    ///
    /// `char_len` must be accurate.
    pub unsafe fn new_with_char_len(data: Box<Wtf8>, kind: StrKind, char_len: usize) -> Self {
        Self {
            data,
            kind,
            len: char_len.into(),
        }
    }

    #[inline]
    pub fn as_wtf8(&self) -> &Wtf8 {
        &self.data
    }

    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        self.kind
            .is_utf8()
            .then(|| unsafe { std::str::from_utf8_unchecked(self.data.as_bytes()) })
    }

    pub fn as_ascii(&self) -> Option<&AsciiStr> {
        self.kind
            .is_ascii()
            .then(|| unsafe { AsciiStr::from_ascii_unchecked(self.data.as_bytes()) })
    }

    pub fn kind(&self) -> StrKind {
        self.kind
    }

    #[inline]
    pub fn as_str_kind(&self) -> PyKindStr<'_> {
        match self.kind {
            StrKind::Ascii => {
                PyKindStr::Ascii(unsafe { AsciiStr::from_ascii_unchecked(self.data.as_bytes()) })
            }
            StrKind::Utf8 => {
                PyKindStr::Utf8(unsafe { std::str::from_utf8_unchecked(self.data.as_bytes()) })
            }
            StrKind::Wtf8 => PyKindStr::Wtf8(&self.data),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    #[inline]
    pub fn char_len(&self) -> usize {
        match self.len.0.load(Relaxed) {
            usize::MAX => self._compute_char_len(),
            len => len,
        }
    }

    #[cold]
    fn _compute_char_len(&self) -> usize {
        let len = if let Some(s) = self.as_str() {
            // utf8 chars().count() is optimized
            s.chars().count()
        } else {
            self.data.code_points().count()
        };
        // len cannot be usize::MAX, since vec.capacity() < sys.maxsize
        self.len.0.store(len, Relaxed);
        len
    }

    pub fn nth_char(&self, index: usize) -> CodePoint {
        match self.as_str_kind() {
            PyKindStr::Ascii(s) => s[index].into(),
            PyKindStr::Utf8(s) => s.chars().nth(index).unwrap().into(),
            PyKindStr::Wtf8(w) => w.code_points().nth(index).unwrap(),
        }
    }
}

impl std::fmt::Display for StrData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.data.fmt(f)
    }
}

impl CharLen for StrData {
    fn char_len(&self) -> usize {
        self.char_len()
    }
}

pub fn try_get_chars(s: &str, range: impl RangeBounds<usize>) -> Option<&str> {
    let mut chars = s.chars();
    let start = match range.start_bound() {
        Bound::Included(&i) => i,
        Bound::Excluded(&i) => i + 1,
        Bound::Unbounded => 0,
    };
    for _ in 0..start {
        chars.next()?;
    }
    let s = chars.as_str();
    let range_len = match range.end_bound() {
        Bound::Included(&i) => i + 1 - start,
        Bound::Excluded(&i) => i - start,
        Bound::Unbounded => return Some(s),
    };
    char_range_end(s, range_len).map(|end| &s[..end])
}

pub fn get_chars(s: &str, range: impl RangeBounds<usize>) -> &str {
    try_get_chars(s, range).unwrap()
}

#[inline]
pub fn char_range_end(s: &str, nchars: usize) -> Option<usize> {
    let i = match nchars.checked_sub(1) {
        Some(last_char_index) => {
            let (index, c) = s.char_indices().nth(last_char_index)?;
            index + c.len_utf8()
        }
        None => 0,
    };
    Some(i)
}

pub fn try_get_codepoints(w: &Wtf8, range: impl RangeBounds<usize>) -> Option<&Wtf8> {
    let mut chars = w.code_points();
    let start = match range.start_bound() {
        Bound::Included(&i) => i,
        Bound::Excluded(&i) => i + 1,
        Bound::Unbounded => 0,
    };
    for _ in 0..start {
        chars.next()?;
    }
    let s = chars.as_wtf8();
    let range_len = match range.end_bound() {
        Bound::Included(&i) => i + 1 - start,
        Bound::Excluded(&i) => i - start,
        Bound::Unbounded => return Some(s),
    };
    codepoint_range_end(s, range_len).map(|end| &s[..end])
}

pub fn get_codepoints(w: &Wtf8, range: impl RangeBounds<usize>) -> &Wtf8 {
    try_get_codepoints(w, range).unwrap()
}

#[inline]
pub fn codepoint_range_end(s: &Wtf8, nchars: usize) -> Option<usize> {
    let i = match nchars.checked_sub(1) {
        Some(last_char_index) => {
            let (index, c) = s.code_point_indices().nth(last_char_index)?;
            index + c.len_wtf8()
        }
        None => 0,
    };
    Some(i)
}

pub fn zfill(bytes: &[u8], width: usize) -> Vec<u8> {
    if width <= bytes.len() {
        bytes.to_vec()
    } else {
        let (sign, s) = match bytes.first() {
            Some(_sign @ b'+') | Some(_sign @ b'-') => {
                (unsafe { bytes.get_unchecked(..1) }, &bytes[1..])
            }
            _ => (&b""[..], bytes),
        };
        let mut filled = Vec::new();
        filled.extend_from_slice(sign);
        filled.extend(std::iter::repeat(b'0').take(width - bytes.len()));
        filled.extend_from_slice(s);
        filled
    }
}

/// Convert a string to ascii compatible, escaping unicodes into escape
/// sequences.
pub fn to_ascii(value: &str) -> AsciiString {
    let mut ascii = Vec::new();
    for c in value.chars() {
        if c.is_ascii() {
            ascii.push(c as u8);
        } else {
            let c = c as i64;
            let hex = if c < 0x100 {
                format!("\\x{c:02x}")
            } else if c < 0x10000 {
                format!("\\u{c:04x}")
            } else {
                format!("\\U{c:08x}")
            };
            ascii.append(&mut hex.into_bytes());
        }
    }
    unsafe { AsciiString::from_ascii_unchecked(ascii) }
}

pub mod levenshtein {
    use std::{cell::RefCell, thread_local};

    pub const MOVE_COST: usize = 2;
    const CASE_COST: usize = 1;
    const MAX_STRING_SIZE: usize = 40;

    fn substitution_cost(mut a: u8, mut b: u8) -> usize {
        if (a & 31) != (b & 31) {
            return MOVE_COST;
        }
        if a == b {
            return 0;
        }
        if a.is_ascii_uppercase() {
            a += b'a' - b'A';
        }
        if b.is_ascii_uppercase() {
            b += b'a' - b'A';
        }
        if a == b { CASE_COST } else { MOVE_COST }
    }

    pub fn levenshtein_distance(a: &str, b: &str, max_cost: usize) -> usize {
        thread_local! {
            static BUFFER: RefCell<[usize; MAX_STRING_SIZE]> = const { RefCell::new([0usize; MAX_STRING_SIZE]) };
        }

        if a == b {
            return 0;
        }

        let (mut a_bytes, mut b_bytes) = (a.as_bytes(), b.as_bytes());
        let (mut a_begin, mut a_end) = (0usize, a.len());
        let (mut b_begin, mut b_end) = (0usize, b.len());

        while a_end > 0 && b_end > 0 && (a_bytes[a_begin] == b_bytes[b_begin]) {
            a_begin += 1;
            b_begin += 1;
            a_end -= 1;
            b_end -= 1;
        }
        while a_end > 0
            && b_end > 0
            && (a_bytes[a_begin + a_end - 1] == b_bytes[b_begin + b_end - 1])
        {
            a_end -= 1;
            b_end -= 1;
        }
        if a_end == 0 || b_end == 0 {
            return (a_end + b_end) * MOVE_COST;
        }
        if a_end > MAX_STRING_SIZE || b_end > MAX_STRING_SIZE {
            return max_cost + 1;
        }

        if b_end < a_end {
            std::mem::swap(&mut a_bytes, &mut b_bytes);
            std::mem::swap(&mut a_begin, &mut b_begin);
            std::mem::swap(&mut a_end, &mut b_end);
        }

        if (b_end - a_end) * MOVE_COST > max_cost {
            return max_cost + 1;
        }

        BUFFER.with(|buffer| {
            let mut buffer = buffer.borrow_mut();
            for i in 0..a_end {
                buffer[i] = (i + 1) * MOVE_COST;
            }

            let mut result = 0usize;
            for (b_index, b_code) in b_bytes[b_begin..(b_begin + b_end)].iter().enumerate() {
                result = b_index * MOVE_COST;
                let mut distance = result;
                let mut minimum = usize::MAX;
                for (a_index, a_code) in a_bytes[a_begin..(a_begin + a_end)].iter().enumerate() {
                    let substitute = distance + substitution_cost(*b_code, *a_code);
                    distance = buffer[a_index];
                    let insert_delete = usize::min(result, distance) + MOVE_COST;
                    result = usize::min(insert_delete, substitute);

                    buffer[a_index] = result;
                    if result < minimum {
                        minimum = result;
                    }
                }
                if minimum > max_cost {
                    return max_cost + 1;
                }
            }
            result
        })
    }
}

/// Replace all tabs in a string with spaces, using the given tab size.
pub fn expandtabs(input: &str, tab_size: usize) -> String {
    let tab_stop = tab_size;
    let mut expanded_str = String::with_capacity(input.len());
    let mut tab_size = tab_stop;
    let mut col_count = 0usize;
    for ch in input.chars() {
        match ch {
            '\t' => {
                let num_spaces = tab_size - col_count;
                col_count += num_spaces;
                let expand = " ".repeat(num_spaces);
                expanded_str.push_str(&expand);
            }
            '\r' | '\n' => {
                expanded_str.push(ch);
                col_count = 0;
                tab_size = 0;
            }
            _ => {
                expanded_str.push(ch);
                col_count += 1;
            }
        }
        if col_count >= tab_size {
            tab_size += tab_stop;
        }
    }
    expanded_str
}

/// Creates an [`AsciiStr`][ascii::AsciiStr] from a string literal, throwing a compile error if the
/// literal isn't actually ascii.
///
/// ```compile_fail
/// # use rustpython_common::str::ascii;
/// ascii!("I ❤️ Rust & Python");
/// ```
#[macro_export]
macro_rules! ascii {
    ($x:expr $(,)?) => {{
        let s = const {
            let s: &str = $x;
            assert!(s.is_ascii(), "ascii!() argument is not an ascii string");
            s
        };
        unsafe { $crate::vendored::ascii::AsciiStr::from_ascii_unchecked(s.as_bytes()) }
    }};
}
pub use ascii;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_chars() {
        let s = "0123456789";
        assert_eq!(get_chars(s, 3..7), "3456");
        assert_eq!(get_chars(s, 3..7), &s[3..7]);

        let s = "0유니코드 문자열9";
        assert_eq!(get_chars(s, 3..7), "코드 문");

        let s = "0😀😃😄😁😆😅😂🤣9";
        assert_eq!(get_chars(s, 3..7), "😄😁😆😅");
    }
}
