//! Mímir registry HTTP client — fetch `.rune` archives and sparse-index entries.
//!
//! Implements the wire-side of fetching package artifacts from a Mímir registry
//! (HTTPS) or a local `file://` mirror. Implements §2 (reference registry domain),
//! §7 (lockfile `source` field), and §8.1 (`edda add` fetch flow) of
//! `corpus/edda-codex/language/08-packages.md`.
//!
//! # Transport
//!
//! Uses `ureq` (sync, rustls-backed TLS, no tokio) to match the daemon
//! concurrency model: sync core + parking_lot + worker threads. Never introduces
//! a tokio runtime.
//!
//! # Wire formats
//!
//! - **HTTPS archive:** GET `<source>/api/v1/xlibs/<name>/<version>/download`
//! - **HTTPS index:** GET `<source>/index/<2-char-prefix>/<name>.json` (NDJSON)
//! - **File archive:** read `<source>/<name>-<version>.rune`
//! - **File index:** read `<source>/index/<prefix>/<name>.json` (same NDJSON)
//!
//! # Crate layout
//!
//! - [`source`] — `RegistrySource` + `ParseError`
//! - [`index`] — `IndexEntry`
//! - [`cap`] — `NetworkCap` placeholder
//! - [`error`] — `FetchError`
//! - [`fetch`] — `fetch_archive` + `fetch_index`

mod cap;
mod error;
mod fetch;
mod index;
mod source;

pub use cap::NetworkCap;
pub use error::FetchError;
pub use fetch::{fetch_archive, fetch_index};
pub use index::IndexEntry;
pub use source::{ParseError, RegistrySource};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Test 1: RegistrySource::parse round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn parse_bare_host() {
        let src = RegistrySource::parse("mimir.edda.dev").unwrap();
        assert_eq!(src, RegistrySource::Https("mimir.edda.dev".to_owned()));
    }

    #[test]
    fn parse_https_url() {
        let src = RegistrySource::parse("https://mirror.acme.com").unwrap();
        assert_eq!(
            src,
            RegistrySource::Https("https://mirror.acme.com".to_owned())
        );
    }

    #[test]
    fn parse_file_url() {
        // Use a platform-appropriate absolute path in the file:// URL.
        #[cfg(unix)]
        {
            let src = RegistrySource::parse("file:///tmp/my-mirror").unwrap();
            assert_eq!(
                src,
                RegistrySource::File(std::path::PathBuf::from("/tmp/my-mirror"))
            );
        }
        #[cfg(windows)]
        {
            let src = RegistrySource::parse("file:///C:/my-mirror").unwrap();
            assert_eq!(
                src,
                RegistrySource::File(std::path::PathBuf::from("C:/my-mirror"))
            );
        }
    }

    #[test]
    fn parse_abs_path_unix() {
        // On Unix, a bare "/" prefix is an absolute path.
        // On Windows, verify a drive-letter absolute path round-trips.
        #[cfg(unix)]
        {
            let src = RegistrySource::parse("/home/user/edda-mirror").unwrap();
            assert_eq!(
                src,
                RegistrySource::File(std::path::PathBuf::from("/home/user/edda-mirror"))
            );
        }
        #[cfg(windows)]
        {
            let src = RegistrySource::parse("C:/tools/edda-mirror").unwrap();
            assert_eq!(
                src,
                RegistrySource::File(std::path::PathBuf::from("C:/tools/edda-mirror"))
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test 2: fetch_archive (file mode) — success
    // -----------------------------------------------------------------------

    #[test]
    fn fetch_archive_file_success() {
        let dir = tempfile::tempdir().unwrap();
        let rune_path = dir.path().join("regex-1.0.0.rune");
        let payload = b"fake rune content";
        std::fs::write(&rune_path, payload).unwrap();

        let source = RegistrySource::File(dir.path().to_path_buf());
        let bytes = fetch_archive(&source, "regex", "1.0.0", NetworkCap).unwrap();
        assert_eq!(bytes, payload.as_slice());
    }

    // -----------------------------------------------------------------------
    // Test 3: fetch_archive (file mode) — not found maps to FetchError::NotFound
    // -----------------------------------------------------------------------

    #[test]
    fn fetch_archive_file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let source = RegistrySource::File(dir.path().to_path_buf());

        let err = fetch_archive(&source, "missing-pkg", "0.0.1", NetworkCap)
            .expect_err("should fail for missing file");

        assert!(
            matches!(err, FetchError::NotFound),
            "expected FetchError::NotFound, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: fetch_index (file mode) — success with valid + malformed lines
    //
    // Design choice: strict — any malformed NDJSON line returns MalformedIndex.
    // Rationale: a registry emitting corrupt data should be flagged loudly
    // rather than silently hiding version availability. The caller can decide
    // to downgrade the error (e.g. fall back to a secondary source).
    // -----------------------------------------------------------------------

    fn make_valid_entry(name: &str, version: &str) -> String {
        serde_json::to_string(&IndexEntry {
            name: name.to_owned(),
            version: version.to_owned(),
            rune_hash: format!("blake3:{}", "a".repeat(64)),
            surface_hash: format!("blake3:{}", "b".repeat(64)),
            effect_hash: format!("blake3:{}", "c".repeat(64)),
            publisher_key: "ed25519:deadbeef".to_owned(),
            publisher_sig: "ed25519:cafebabe".to_owned(),
        })
        .unwrap()
    }

    #[test]
    fn fetch_index_file_two_valid_entries() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = "re";
        let index_dir = dir.path().join("index").join(prefix);
        std::fs::create_dir_all(&index_dir).unwrap();

        let entry1 = make_valid_entry("regex", "1.0.0");
        let entry2 = make_valid_entry("regex", "2.0.0");
        let ndjson = format!("{entry1}\n{entry2}\n");
        std::fs::write(index_dir.join("regex.json"), &ndjson).unwrap();

        let source = RegistrySource::File(dir.path().to_path_buf());
        let entries = fetch_index(&source, "regex", NetworkCap).unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].version, "1.0.0");
        assert_eq!(entries[1].version, "2.0.0");
    }

    #[test]
    fn fetch_index_file_malformed_line_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = "re";
        let index_dir = dir.path().join("index").join(prefix);
        std::fs::create_dir_all(&index_dir).unwrap();

        let entry1 = make_valid_entry("regex", "1.0.0");
        let entry2 = make_valid_entry("regex", "2.0.0");
        // One malformed line between two valid ones.
        let ndjson = format!("{entry1}\nnot valid json\n{entry2}\n");
        std::fs::write(index_dir.join("regex.json"), &ndjson).unwrap();

        let source = RegistrySource::File(dir.path().to_path_buf());
        let err = fetch_index(&source, "regex", NetworkCap)
            .expect_err("should fail on malformed line");

        assert!(
            matches!(err, FetchError::MalformedIndex(_)),
            "expected FetchError::MalformedIndex, got: {err}"
        );
    }

    #[test]
    fn fetch_index_file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let source = RegistrySource::File(dir.path().to_path_buf());

        let err = fetch_index(&source, "no-such-pkg", NetworkCap)
            .expect_err("should fail for missing index file");

        assert!(
            matches!(err, FetchError::NotFound),
            "expected FetchError::NotFound, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Note: HTTPS mode coverage is deferred to integration tests in slice I.
    // A `tiny_http`-backed test server would require adding `tiny_http` as a
    // dev-dependency; the sync file-mode tests above cover all parse/error
    // paths. The HTTPS code path exercises the same `parse_ndjson`,
    // `index_prefix`, and error-mapping logic — only the `ureq::get` call
    // differs, which integration tests in slice I will cover against a real or
    // stub HTTPS server.
    // -----------------------------------------------------------------------
}
