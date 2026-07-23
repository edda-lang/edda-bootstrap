//! Registry source descriptor — where a Mímir registry lives.

use std::path::PathBuf;

/// Error produced when a registry source string cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The `file://` URL contains a non-absolute path or is otherwise malformed.
    BadFilePath(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::BadFilePath(msg) => write!(f, "invalid file path: {msg}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Where an artifact or index lives — either an HTTPS Mímir host or a local directory.
///
/// Lockfile's `source` field deserialises into this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrySource {
    /// An HTTPS Mímir registry. Stores the host+path as given in the lockfile,
    /// e.g. `"mimir.edda.dev"` or `"https://mirror.acme.com"`.
    Https(String),
    /// A local directory mirror. Holds an absolute path to a dir
    /// containing `.rune` files and an `index/` subdirectory.
    File(PathBuf),
}

impl RegistrySource {
    /// Parse from the lockfile's text form.
    ///
    /// - `"mimir.edda.dev"`          → `Https("mimir.edda.dev")`
    /// - `"https://mirror.acme.com"` → `Https("https://mirror.acme.com")`
    /// - `"file:///path/to/dir"`     → `File(PathBuf::from("/path/to/dir"))`
    /// - `"/abs/path"`               → `File(PathBuf::from("/abs/path"))`
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        if s.starts_with("file://") {
            // RFC 3986: file:///path/to/dir has three slashes.
            // Strip the "file://" prefix; the remainder should start with "/".
            // On Windows we may get file:///C:/path — after stripping "file://"
            // we get "/C:/path". We detect that and strip the leading "/" so
            // PathBuf::from gives us "C:/path" which is_absolute on Windows.
            let after_scheme = s.trim_start_matches("file://");
            let path_str = strip_file_url_lead(after_scheme);
            let path = PathBuf::from(path_str);
            if !path.is_absolute() {
                return Err(ParseError::BadFilePath(format!(
                    "file:// URL must have an absolute path, got: {s}"
                )));
            }
            return Ok(RegistrySource::File(path));
        }

        // Unix / Windows absolute paths
        if is_absolute_path(s) {
            return Ok(RegistrySource::File(PathBuf::from(s)));
        }

        // Everything else is treated as an HTTPS registry host (bare or with scheme).
        Ok(RegistrySource::Https(s.to_owned()))
    }

    /// Return the effective base URL for HTTPS requests.
    ///
    /// If the stored string already contains a scheme, it is returned unchanged.
    /// Otherwise `"https://"` is prepended.
    pub(crate) fn https_base(&self) -> Option<String> {
        match self {
            RegistrySource::Https(raw) => {
                if raw.starts_with("https://") || raw.starts_with("http://") {
                    Some(raw.clone())
                } else {
                    Some(format!("https://{raw}"))
                }
            }
            RegistrySource::File(_) => None,
        }
    }
}

/// Strip the leading "/" from a `file://`-decoded path on Windows.
///
/// On Windows a `file:///C:/path` URL decodes to `/C:/path` after stripping
/// the `file://` prefix. The leading `/` makes the path non-absolute by Rust's
/// `Path::is_absolute()` convention. We detect the Windows drive-letter pattern
/// (`/X:/`) and strip the leading slash; on Unix the path is already correct.
fn strip_file_url_lead(after_scheme: &str) -> &str {
    // Detect /X:/ or /X:\ at the start, which indicates a Windows drive.
    if after_scheme.len() >= 4 {
        let bytes = after_scheme.as_bytes();
        let is_win_drive = bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && bytes[2] == b':'
            && (bytes[3] == b'/' || bytes[3] == b'\\');
        if is_win_drive {
            return &after_scheme[1..];
        }
    }
    after_scheme
}

/// Return true if `s` looks like an absolute filesystem path.
fn is_absolute_path(s: &str) -> bool {
    // Unix absolute paths start with '/'
    if s.starts_with('/') {
        return true;
    }
    // Windows absolute paths: drive letters like C:\ or C:/
    if s.len() >= 3 {
        let bytes = s.as_bytes();
        let is_drive = bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/');
        if is_drive {
            return true;
        }
    }
    false
}
