//! Pack and unpack of the `.rune` tar.zst archive format.
//!
//! Implements §3 of `corpus/edda-codex/language/08-packages.md`:
//! - tar ordering: entries sorted lex by path, mtimes zeroed, uid/gid zeroed.
//! - zstd flags: `--long=27 -19` (window_log=27, level=19, long-distance mode).
//! - Per-file BLAKE3 verification against `hashes.toon` on unpack.
//!
//! Signature and publisher-key verification are NOT performed here — that is
//! slice E (ed25519) + slice F's responsibility. This crate's contract is
//! bytes-in / bytes-out with per-file hash integrity.

mod error;
mod hash_toon;
mod pack;
mod unpack;

pub use error::{PackError, UnpackError};
pub use hash_toon::build_hashes_toon;

/// Canonical layout of an unpacked `.rune`, matching `08-packages.md` §3.2.
#[derive(Debug)]
pub struct RuneLayout {
    /// Canonical subset of `package.toml`.
    pub manifest_toml: Vec<u8>,
    /// `(module-leaf, .toon bytes)` sorted lex by module-leaf.
    pub surface: Vec<(String, Vec<u8>)>,
    /// `(module-leaf, .mir bytes)` sorted lex by module-leaf.
    pub mir: Vec<(String, Vec<u8>)>,
    /// `(triple, leaf, bytes)` sorted lex by `(triple, leaf)`.
    pub objects: Vec<(String, String, Vec<u8>)>,
    /// Compiler-emitted directory structmap (`index.toon`).
    pub index_toon: Vec<u8>,
    /// Per-file BLAKE3s plus the three rollup hashes (`hashes.toon`).
    pub hashes_toon: Vec<u8>,
    /// Ed25519 signature over `hashes.toon` canonical bytes (`signature.bin`).
    pub signature_bin: Vec<u8>,
    /// Publisher pubkey + fingerprint line, text form (`publisher.key`).
    pub publisher_key: Vec<u8>,
}

/// Pack a layout into a single `.rune` byte stream.
///
/// Tar entries are sorted lex by path. All entry metadata (mtime, uid, gid)
/// is zeroed. Zstd is compressed at level 19 with a 128 MiB window
/// (`--long=27`) per spec §3.1. Two calls with the same `layout` produce
/// byte-identical output.
pub fn pack(layout: &RuneLayout) -> Result<Vec<u8>, PackError> {
    pack::pack(layout)
}

/// Unpack a `.rune` byte stream, verifying every per-file BLAKE3 against
/// `hashes.toon`.
///
/// Decompresses the full zstd stream into memory, extracts `hashes.toon`
/// in a first pass, parses its `files[...]` table into a `path → blake3:<hex>`
/// map, then makes a second pass over the buffer verifying each entry before
/// accepting it into the returned `RuneLayout`.
pub fn unpack(bytes: &[u8]) -> Result<RuneLayout, UnpackError> {
    unpack::unpack(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash_toon::build_hashes_toon;

    fn sample_layout() -> RuneLayout {
        let manifest = b"[package]\nname = \"sample\"\n".to_vec();
        let surf = vec![
            ("alpha".to_string(), b"alpha surface".to_vec()),
            ("beta".to_string(), b"beta surface".to_vec()),
        ];
        let mir_entries = vec![
            ("alpha".to_string(), b"alpha mir".to_vec()),
            ("beta".to_string(), b"beta mir".to_vec()),
        ];
        let objs = vec![
            (
                "x86_64-pc-windows-msvc".to_string(),
                "alpha.o".to_string(),
                b"alpha obj bytes".to_vec(),
            ),
        ];
        let index = b"# index.toon\nschema_version: 3\n".to_vec();
        let signature = b"fake-sig-bytes".to_vec();
        let pubkey = b"ed25519:aabbccdd\nfingerprint: ed25519:aabbccdd\n".to_vec();

        // Build hashes_toon from actual content.
        let hashes = build_hashes_toon(&manifest, &surf, &mir_entries, &objs, &index);

        RuneLayout {
            manifest_toml: manifest,
            surface: surf,
            mir: mir_entries,
            objects: objs,
            index_toon: index,
            hashes_toon: hashes,
            signature_bin: signature,
            publisher_key: pubkey,
        }
    }

    #[test]
    fn pack_then_unpack_round_trip() {
        let layout = sample_layout();
        let bytes = pack(&layout).expect("pack failed");
        let unpacked = unpack(&bytes).expect("unpack failed");

        assert_eq!(unpacked.manifest_toml, layout.manifest_toml);
        assert_eq!(unpacked.surface, layout.surface);
        assert_eq!(unpacked.mir, layout.mir);
        assert_eq!(unpacked.objects, layout.objects);
        assert_eq!(unpacked.index_toon, layout.index_toon);
        assert_eq!(unpacked.hashes_toon, layout.hashes_toon);
        assert_eq!(unpacked.signature_bin, layout.signature_bin);
        assert_eq!(unpacked.publisher_key, layout.publisher_key);
    }

    #[test]
    fn pack_is_deterministic() {
        let layout = sample_layout();
        let a = pack(&layout).unwrap();
        let b = pack(&layout).unwrap();
        assert_eq!(a, b, "pack must be byte-identical across two calls");
    }

    #[test]
    fn unpack_rejects_missing_hashes_toon() {
        // Build a tar.zst without hashes.toon.
        use std::io::Write as _;

        let zstd_bytes = {
            let mut tar_bytes = Vec::new();
            {
                let mut builder = tar::Builder::new(&mut tar_bytes);
                let content = b"[package]\nname=\"x\"\n";
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_mtime(0);
                header.set_uid(0);
                header.set_gid(0);
                header.set_cksum();
                builder
                    .append_data(&mut header, "manifest.toml", content.as_ref())
                    .unwrap();
                builder.finish().unwrap();
            }
            let mut zstd_out = Vec::new();
            let mut enc = zstd::stream::write::Encoder::new(&mut zstd_out, 1).unwrap();
            enc.write_all(&tar_bytes).unwrap();
            enc.finish().unwrap();
            zstd_out
        };

        let err = unpack(&zstd_bytes).expect_err("should fail without hashes.toon");
        assert!(
            matches!(err, UnpackError::MissingHashesToon),
            "expected MissingHashesToon, got {:?}",
            err
        );
    }

    #[test]
    fn unpack_rejects_hash_mismatch() {
        let mut layout = sample_layout();
        // Poison manifest_toml bytes after hashes_toon was computed.
        layout.manifest_toml = b"POISONED CONTENT\n".to_vec();

        let bytes = pack(&layout).expect("pack failed");
        let err = unpack(&bytes).expect_err("should fail on hash mismatch");
        assert!(
            matches!(err, UnpackError::HashMismatch { .. }),
            "expected HashMismatch, got {:?}",
            err
        );
    }

    #[test]
    fn unpack_rejects_unexpected_entry() {
        // Pack normally then inject an extra entry at the tar level.
        use std::io::Write as _;

        let layout = sample_layout();
        let packed = pack(&layout).unwrap();

        // Decompress the archive, append an extra entry, recompress.
        let tar_bytes = {
            let cursor = std::io::Cursor::new(&packed);
            let mut dec = zstd::stream::read::Decoder::new(cursor).unwrap();
            let mut buf = Vec::new();
            std::io::copy(&mut dec, &mut buf).unwrap();
            buf
        };

        // Append an extra file before the end-of-archive marker.
        // tar archives end with two 512-byte zero blocks. We insert our entry
        // just before those.
        let eoa_start = tar_bytes.len().saturating_sub(1024);
        let mut new_tar = tar_bytes[..eoa_start].to_vec();
        {
            let mut builder = tar::Builder::new(&mut new_tar);
            let content = b"unexpected";
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_cksum();
            builder
                .append_data(&mut header, "rogue.txt", content.as_ref())
                .unwrap();
            builder.finish().unwrap();
        }

        let new_zstd = {
            let mut out = Vec::new();
            let mut enc = zstd::stream::write::Encoder::new(&mut out, 1).unwrap();
            enc.write_all(&new_tar).unwrap();
            enc.finish().unwrap();
            out
        };

        let err = unpack(&new_zstd).expect_err("should reject unexpected entry");
        assert!(
            matches!(err, UnpackError::UnexpectedEntry(_)),
            "expected UnexpectedEntry, got {:?}",
            err
        );
    }

    #[test]
    fn tar_canonicalisation() {
        let layout = sample_layout();
        let bytes = pack(&layout).unwrap();

        // Decompress and walk the tar headers.
        let cursor = std::io::Cursor::new(&bytes);
        let mut dec = zstd::stream::read::Decoder::new(cursor).unwrap();
        let mut tar_bytes = Vec::new();
        std::io::copy(&mut dec, &mut tar_bytes).unwrap();

        let cursor2 = std::io::Cursor::new(tar_bytes);
        let mut archive = tar::Archive::new(cursor2);
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let h = entry.header();
            assert_eq!(h.mtime().unwrap(), 0, "mtime must be zero");
            assert_eq!(h.uid().unwrap(), 0, "uid must be zero");
            assert_eq!(h.gid().unwrap(), 0, "gid must be zero");
        }
    }
}
