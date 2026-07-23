//! Core fetch logic for HTTPS and file-mode registry sources.

use crate::cap::NetworkCap;
use crate::error::FetchError;
use crate::index::IndexEntry;
use crate::source::RegistrySource;

// ---------------------------------------------------------------------------
// Archive fetch
// ---------------------------------------------------------------------------

/// Fetch the raw `.rune` archive bytes for `name@version`.
///
/// Returns the raw bytes; the caller is responsible for verifying
/// `rune_hash` and the publisher signature from the index entry.
///
/// - HTTPS: GET `<source>/api/v1/xlibs/<name>/<version>/download`
/// - File: read `<source>/<name>-<version>.rune`
pub fn fetch_archive(
    source: &RegistrySource,
    name: &str,
    version: &str,
    _network_cap: NetworkCap,
) -> Result<Vec<u8>, FetchError> {
    match source {
        RegistrySource::Https(_) => fetch_archive_https(source, name, version),
        RegistrySource::File(dir) => fetch_archive_file(dir, name, version),
    }
}

// ---------------------------------------------------------------------------
// Index fetch
// ---------------------------------------------------------------------------

/// Fetch every published version's index entry for a package.
///
/// - HTTPS: GET `<source>/index/<prefix>/<name>.json` (NDJSON, one `IndexEntry` per line)
/// - File: read `<source>/index/<prefix>/<name>.json` (same NDJSON format)
///
/// **Partial-line behaviour (file mode):** if one line fails to parse, the
/// function returns `Err(FetchError::MalformedIndex(...))` immediately. This
/// is the stricter choice: a registry that emits corrupt index data should be
/// flagged loudly rather than silently skipping entries, which could mask
/// version availability gaps. The caller can decide whether to downgrade the
/// error.
pub fn fetch_index(
    source: &RegistrySource,
    name: &str,
    _network_cap: NetworkCap,
) -> Result<Vec<IndexEntry>, FetchError> {
    let ndjson = match source {
        RegistrySource::Https(_) => fetch_index_bytes_https(source, name)?,
        RegistrySource::File(dir) => fetch_index_bytes_file(dir, name)?,
    };
    parse_ndjson(&ndjson)
}

// ---------------------------------------------------------------------------
// HTTPS helpers
// ---------------------------------------------------------------------------

/// GET the archive from the HTTPS registry.
fn fetch_archive_https(
    source: &RegistrySource,
    name: &str,
    version: &str,
) -> Result<Vec<u8>, FetchError> {
    let base = source.https_base().expect("Https variant always has a base");
    let url = format!("{base}/api/v1/xlibs/{name}/{version}/download");
    http_get_bytes(&url)
}

/// GET the index NDJSON from the HTTPS registry.
fn fetch_index_bytes_https(source: &RegistrySource, name: &str) -> Result<String, FetchError> {
    let base = source.https_base().expect("Https variant always has a base");
    let prefix = index_prefix(name);
    let url = format!("{base}/index/{prefix}/{name}.json");
    let bytes = http_get_bytes(&url)?;
    String::from_utf8(bytes)
        .map_err(|e| FetchError::MalformedIndex(format!("index response is not UTF-8: {e}")))
}

/// Perform a synchronous HTTP GET and return the body bytes.
///
/// Uses `ureq` (no tokio, rustls-based TLS). Any non-2xx status maps to
/// `FetchError::Http`; transport failures map to `FetchError::Network`.
/// 404 is mapped to `FetchError::NotFound`.
fn http_get_bytes(url: &str) -> Result<Vec<u8>, FetchError> {
    let resp = ureq::get(url).call().map_err(|e| {
        use ureq::Error;
        match e {
            Error::Status(status, resp) => {
                if status == 404 {
                    return FetchError::NotFound;
                }
                let body = resp.into_string().unwrap_or_default();
                FetchError::Http {
                    status,
                    msg: body,
                }
            }
            Error::Transport(t) => FetchError::Network(t.to_string()),
        }
    })?;

    let status = resp.status();
    if status == 404 {
        return Err(FetchError::NotFound);
    }
    if !(200..300).contains(&status) {
        let body = resp.into_string().unwrap_or_default();
        return Err(FetchError::Http {
            status,
            msg: body,
        });
    }

    let mut buf: Vec<u8> = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .map_err(|e| FetchError::Network(format!("failed to read response body: {e}")))?;
    Ok(buf)
}

// ---------------------------------------------------------------------------
// File-mode helpers
// ---------------------------------------------------------------------------

/// Read the archive from a local file-mode mirror.
fn fetch_archive_file(dir: &std::path::Path, name: &str, version: &str) -> Result<Vec<u8>, FetchError> {
    let path = dir.join(format!("{name}-{version}.rune"));
    read_file_to_bytes(&path)
}

/// Read the index NDJSON from a local file-mode mirror.
fn fetch_index_bytes_file(dir: &std::path::Path, name: &str) -> Result<String, FetchError> {
    let prefix = index_prefix(name);
    let path = dir.join("index").join(&prefix).join(format!("{name}.json"));
    let bytes = read_file_to_bytes(&path)?;
    String::from_utf8(bytes)
        .map_err(|e| FetchError::MalformedIndex(format!("index file is not UTF-8: {e}")))
}

/// Read a file's entire contents as bytes.
///
/// Maps `ErrorKind::NotFound` to `FetchError::NotFound`; all other I/O errors
/// become `FetchError::Io`.
fn read_file_to_bytes(path: &std::path::Path) -> Result<Vec<u8>, FetchError> {
    std::fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            FetchError::NotFound
        } else {
            FetchError::Io(e)
        }
    })
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Compute the 2-char (or shorter) sparse-index prefix for a package name.
///
/// Follows cargo's sparse-index convention: take the first 2 characters.
/// Names shorter than 2 characters use the name itself as the prefix.
fn index_prefix(name: &str) -> String {
    name.chars().take(2).collect()
}

/// Parse newline-delimited JSON into a `Vec<IndexEntry>`.
///
/// Strict: any line that fails to parse returns `Err(FetchError::MalformedIndex)`.
/// Empty lines are skipped.
fn parse_ndjson(ndjson: &str) -> Result<Vec<IndexEntry>, FetchError> {
    let mut entries = Vec::new();
    for line in ndjson.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: IndexEntry = serde_json::from_str(line).map_err(|e| {
            FetchError::MalformedIndex(format!("failed to parse index line: {e}\nLine: {line}"))
        })?;
        entries.push(entry);
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------
// std::io::Read import needed for into_reader()
// ---------------------------------------------------------------------------

use std::io::Read as _;
