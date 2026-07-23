//! `.rune` archive packer — tar.zst with pinned zstd flags.
//!
//! Per spec §3.1: tar entries sorted lex by path, mtimes zeroed, uid/gid
//! zeroed, zstd `--long=27 -19` (window_log=27, level=19).

use std::io::Write as _;

use crate::error::PackError;
use crate::RuneLayout;

/// Pack a layout into a `.rune` byte stream.
pub fn pack(layout: &RuneLayout) -> Result<Vec<u8>, PackError> {
    // Collect all (path, bytes) pairs in §3.2 order, then sort lex.
    let mut entries: Vec<(String, &[u8])> = collect_entries(layout);
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Build the tar archive in memory.
    let tar_bytes = build_tar(&entries)?;

    // Compress with zstd flags --long=27 -19.
    //   --long=27 sets window_log=27 (128 MiB window).
    //   -19 is the compression level.
    //   Long-distance matching is implicit when window_log >= 27 in zstd >= 1.3;
    //   the `zstd` Rust crate exposes window_log via `Encoder::window_log`.
    compress_zstd(&tar_bytes)
}

/// Gather all archive entries from the layout. Paths follow §3.2 exactly.
fn collect_entries(layout: &RuneLayout) -> Vec<(String, &[u8])> {
    let mut entries: Vec<(String, &[u8])> = Vec::new();

    entries.push(("manifest.toml".to_string(), &layout.manifest_toml));

    for (leaf, bytes) in &layout.surface {
        entries.push((format!("surface/{leaf}.toon"), bytes.as_slice()));
    }

    for (leaf, bytes) in &layout.mir {
        entries.push((format!("mir/{leaf}.mir"), bytes.as_slice()));
    }

    for (triple, leaf, bytes) in &layout.objects {
        entries.push((format!("objects/{triple}/{leaf}"), bytes.as_slice()));
    }

    entries.push(("index.toon".to_string(), &layout.index_toon));
    entries.push(("hashes.toon".to_string(), &layout.hashes_toon));
    entries.push(("signature.bin".to_string(), &layout.signature_bin));
    entries.push(("publisher.key".to_string(), &layout.publisher_key));

    entries
}

/// Build the raw tar bytes from sorted (path, bytes) pairs.
fn build_tar(entries: &[(String, &[u8])]) -> Result<Vec<u8>, PackError> {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        // Disable automatic header timestamp injection (not needed for GNU headers
        // we build manually, but belt-and-suspenders).
        builder.follow_symlinks(false);

        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_cksum();

            builder
                .append_data(&mut header, path.as_str(), *data)
                .map_err(|e| PackError::Tar(e.to_string()))?;
        }

        builder
            .finish()
            .map_err(|e| PackError::Tar(e.to_string()))?;
    }
    Ok(tar_bytes)
}

/// Compress `tar_bytes` with zstd at level 19, window_log 27.
fn compress_zstd(tar_bytes: &[u8]) -> Result<Vec<u8>, PackError> {
    let mut out = Vec::new();
    let mut encoder = zstd::stream::write::Encoder::new(&mut out, 19)
        .map_err(|e| PackError::Zstd(e.to_string()))?;
    encoder
        .window_log(27)
        .map_err(|e| PackError::Zstd(e.to_string()))?;
    encoder
        .write_all(tar_bytes)
        .map_err(|e| PackError::Zstd(e.to_string()))?;
    encoder
        .finish()
        .map_err(|e| PackError::Zstd(e.to_string()))?;
    Ok(out)
}
