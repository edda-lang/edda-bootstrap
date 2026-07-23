//! Error types for `.rune` pack and unpack operations.

/// Error produced by [`crate::pack`].
#[derive(Debug)]
pub enum PackError {
    /// An I/O error from the underlying byte sink.
    Io(std::io::Error),
    /// A tar builder error (wrapped message string for object safety).
    Tar(String),
    /// A zstd encoder error.
    Zstd(String),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::Io(e) => write!(f, "I/O error during pack: {e}"),
            PackError::Tar(msg) => write!(f, "tar error during pack: {msg}"),
            PackError::Zstd(msg) => write!(f, "zstd error during pack: {msg}"),
        }
    }
}

impl std::error::Error for PackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PackError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for PackError {
    fn from(e: std::io::Error) -> Self {
        PackError::Io(e)
    }
}

/// Error produced by [`crate::unpack`].
#[derive(Debug)]
pub enum UnpackError {
    /// An I/O error from the underlying byte source.
    Io(std::io::Error),
    /// A tar extraction error.
    Tar(String),
    /// A zstd decompression error.
    Zstd(String),
    /// `hashes.toon` was not present in the archive.
    MissingHashesToon,
    /// `hashes.toon` was present but could not be parsed.
    MalformedHashesToon(String),
    /// A file's actual BLAKE3 did not match the value in `hashes.toon`.
    HashMismatch {
        /// Archive-relative path of the mismatched file.
        file: String,
        /// `blake3:<hex>` recorded in `hashes.toon`.
        expected: String,
        /// `blake3:<hex>` computed from the actual bytes.
        actual: String,
    },
    /// A tar entry whose path is not part of the §3.2 schema was encountered.
    UnexpectedEntry(String),
    /// A required §3.2 entry was absent from the archive.
    MissingEntry(String),
}

impl std::fmt::Display for UnpackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnpackError::Io(e) => write!(f, "I/O error during unpack: {e}"),
            UnpackError::Tar(msg) => write!(f, "tar error during unpack: {msg}"),
            UnpackError::Zstd(msg) => write!(f, "zstd error during unpack: {msg}"),
            UnpackError::MissingHashesToon => write!(f, "archive missing required hashes.toon"),
            UnpackError::MalformedHashesToon(msg) => {
                write!(f, "hashes.toon is malformed: {msg}")
            }
            UnpackError::HashMismatch { file, expected, actual } => {
                write!(
                    f,
                    "hash mismatch for {file}: expected {expected}, got {actual}"
                )
            }
            UnpackError::UnexpectedEntry(path) => {
                write!(f, "unexpected tar entry not in §3.2 schema: {path}")
            }
            UnpackError::MissingEntry(path) => {
                write!(f, "required §3.2 entry missing from archive: {path}")
            }
        }
    }
}

impl std::error::Error for UnpackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            UnpackError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for UnpackError {
    fn from(e: std::io::Error) -> Self {
        UnpackError::Io(e)
    }
}
