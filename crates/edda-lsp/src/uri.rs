//! LSP `Uri` ↔ filesystem path conversion.
//!
//! Every LSP `textDocument` carries a `Uri`; the daemon's query surface
//! takes `&Path`. The conversion is best-effort: a `file:` URL maps to
//! an absolute path and back; non-`file:` URIs are surfaced as
//! [`UriError::UnsupportedScheme`] so the LSP handler can reply with a
//! protocol error rather than silently mishandling it.

use std::path::PathBuf;

use lsp_types::Uri;

/// Failure mode for [`uri_to_path`].
#[derive(Debug)]
pub enum UriError {
    /// The URI used a scheme other than `file:` (`http:`, `untitled:`, etc.).
    /// `untitled:` is the canonical example — an LSP client opens a buffer
    /// that has no on-disk path; the daemon's query surface is path-keyed
    /// so we cannot serve it.
    UnsupportedScheme(String),
    /// The URI parsed but produced no usable filesystem path.
    InvalidPath(String),
}

impl std::fmt::Display for UriError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedScheme(s) => write!(f, "unsupported URI scheme: {s}"),
            Self::InvalidPath(s) => write!(f, "URI is not a valid filesystem path: {s}"),
        }
    }
}

impl std::error::Error for UriError {}

/// Convert an LSP [`Uri`] to a [`PathBuf`]. Only `file:` URIs succeed.
pub fn uri_to_path(uri: &Uri) -> Result<PathBuf, UriError> {
    let s = uri.as_str();
    let stripped = match s.strip_prefix("file://") {
        Some(rest) => rest,
        None => {
            let scheme_end = s.find(':').map(|i| &s[..i]).unwrap_or("(none)");
            return Err(UriError::UnsupportedScheme(scheme_end.to_string()));
        }
    };

    let decoded = percent_decode(stripped);
    let path = if cfg!(windows) {
        // file:///C:/path → C:/path  (strip leading `/` if followed by drive letter)
        strip_windows_leader(&decoded)
    } else {
        decoded
    };
    if path.is_empty() {
        return Err(UriError::InvalidPath(s.to_string()));
    }
    Ok(PathBuf::from(path))
}

/// Convert a filesystem path to an LSP [`Uri`] (`file:` scheme).
pub fn path_to_uri(path: &std::path::Path) -> Result<Uri, UriError> {
    let s = path.to_string_lossy();
    let encoded = path_to_uri_string(&s);
    encoded
        .parse::<Uri>()
        .map_err(|_| UriError::InvalidPath(s.into_owned()))
}

/// Compose a `file:` URI string from an absolute or relative path.
fn path_to_uri_string(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    if cfg!(windows) && has_drive_letter(&normalized) {
        format!("file:///{}", percent_encode_path(&normalized))
    } else if let Some(rest) = normalized.strip_prefix('/') {
        format!("file:///{}", percent_encode_path(rest))
    } else {
        format!("file:///{}", percent_encode_path(&normalized))
    }
}

fn has_drive_letter(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// `/C:/foo` → `C:/foo` on Windows. Leaves other paths intact.
fn strip_windows_leader(path: &str) -> String {
    if let Some(rest) = path.strip_prefix('/')
        && has_drive_letter(rest)
    {
        return rest.to_string();
    }
    path.to_string()
}

/// Minimal percent-decode. Handles `%XX` byte triplets; leaves everything
/// else verbatim. We do not pull in `percent-encoding` for one helper.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    // Bounded loop: each iteration advances `i` by at least 1; loop ends at
    // `bytes.len()`.
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

fn percent_encode_path(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    // Bounded loop: one iteration per byte.
    for b in input.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'/'
            | b':' => out.push(b as char),
            _ => {
                out.push('%');
                out.push(hex_digit(b >> 4));
                out.push(hex_digit(b & 0x0F));
            }
        }
    }
    out
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + (b - b'a')),
        b'A'..=b'F' => Some(10 + (b - b'A')),
        _ => None,
    }
}

fn hex_digit(n: u8) -> char {
    match n & 0x0F {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + (n - 10)) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_ascii() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("a%2Fb"), "a/b");
    }

    #[test]
    fn round_trip_simple_path() {
        let path = if cfg!(windows) {
            PathBuf::from("C:/tmp/file.ea")
        } else {
            PathBuf::from("/tmp/file.ea")
        };
        let uri = path_to_uri(&path).expect("encode");
        let back = uri_to_path(&uri).expect("decode");
        assert_eq!(back, path);
    }
}
