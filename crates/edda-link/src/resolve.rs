//! Pre-link runtime-extern resolution.
//!
//! Scans every link input (object file or archive, in link order) for
//! `__edda_*` runtime-extern symbols before the actual linker or archiver
//! runs. A symbol referenced (undefined) in one input but not defined in
//! any input is reported so the driver can emit an attributable
//! `unprovided_runtime_extern` diagnostic instead of letting the
//! underlying tool fail with a bare `undefined symbol` message
//! (mirrors the native compiler's pre-link gate).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use object::read::archive::ArchiveFile;
use object::{File as ObjectFile, Object, ObjectSymbol};
use smol_str::SmolStr;

use crate::error::LinkError;

/// Prefix identifying an `edda-rt` runtime-extern symbol.
pub const RUNTIME_EXTERN_PREFIX: &str = "__edda_";

/// Result of [`resolve_runtime_externs`].
#[derive(Debug)]
pub struct RuntimeExternReport {
    /// Every `__edda_*` symbol referenced (undefined) somewhere in the
    /// scanned inputs but not defined by any of them.
    pub unprovided: Vec<String>,
    /// Every `__edda_*` symbol defined (non-undefined) in more than one
    /// scanned input -- the linker resolves the clash to one definition
    /// arbitrarily (mirroring the native
    /// compiler's `duplicate_runtime_extern` diagnostic).
    pub duplicates: Vec<String>,
}

/// Scan `inputs` (object files and archives, in link order) for
/// `__edda_*` runtime-extern symbols referenced by one input but not
/// defined by any input. Returns the report before any linker runs.
pub fn resolve_runtime_externs(inputs: &[PathBuf]) -> Result<RuntimeExternReport, LinkError> {
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    let mut defined: BTreeSet<String> = BTreeSet::new();
    let mut duplicates: BTreeSet<String> = BTreeSet::new();

    for path in inputs {
        let data = std::fs::read(path).map_err(|source| LinkError::ObjectReadFailed {
            path: path.clone(),
            detail: SmolStr::new(source.to_string()),
        })?;
        scan_input(path, &data, &mut referenced, &mut defined, &mut duplicates)?;
    }

    let unprovided = referenced
        .into_iter()
        .filter(|name| !defined.contains(name))
        .collect();
    Ok(RuntimeExternReport { unprovided, duplicates: duplicates.into_iter().collect() })
}

fn scan_input(
    path: &Path,
    data: &[u8],
    referenced: &mut BTreeSet<String>,
    defined: &mut BTreeSet<String>,
    duplicates: &mut BTreeSet<String>,
) -> Result<(), LinkError> {
    if let Ok(archive) = ArchiveFile::parse(data) {
        for member in archive.members().flatten() {
            let Ok(member_data) = member.data(data) else {
                continue;
            };
            if let Ok(file) = ObjectFile::parse(member_data) {
                scan_object(&file, referenced, defined, duplicates);
            }
        }
        return Ok(());
    }

    let file = ObjectFile::parse(data).map_err(|source| LinkError::ObjectReadFailed {
        path: path.to_path_buf(),
        detail: SmolStr::new(source.to_string()),
    })?;
    scan_object(&file, referenced, defined, duplicates);
    Ok(())
}

fn scan_object(
    file: &ObjectFile<'_>,
    referenced: &mut BTreeSet<String>,
    defined: &mut BTreeSet<String>,
    duplicates: &mut BTreeSet<String>,
) {
    for symbol in file.symbols() {
        let Ok(name) = symbol.name() else {
            continue;
        };
        if !name.starts_with(RUNTIME_EXTERN_PREFIX) {
            continue;
        }
        if symbol.is_undefined() {
            referenced.insert(name.to_string());
        } else if symbol.is_definition() {
            if !defined.insert(name.to_string()) {
                duplicates.insert(name.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Best-effort tempfile that removes itself on drop. Avoids a
    /// `tempfile` dev-dep just for this gate's smoke tests (mirrors
    /// `edda-compile`'s `emit::tests::TempPath`).
    struct TempPath(PathBuf);
    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    impl TempPath {
        fn with_contents(suffix: &str, bytes: &[u8]) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = std::env::temp_dir().join(format!("edda-link-resolve-{pid}-{seq}-{suffix}"));
            std::fs::write(&path, bytes).expect("write temp link input");
            TempPath(path)
        }
        fn as_path_buf(&self) -> PathBuf {
            self.0.clone()
        }
    }

    #[test]
    fn nonexistent_input_is_object_read_failed() {
        let path = PathBuf::from("this/path/does/not/exist.o");
        let err = resolve_runtime_externs(std::slice::from_ref(&path)).unwrap_err();
        assert!(matches!(err, LinkError::ObjectReadFailed { .. }));
    }

    #[test]
    fn garbage_bytes_are_object_read_failed() {
        let temp = TempPath::with_contents("garbage.o", b"not an object file or archive");
        let err = resolve_runtime_externs(&[temp.as_path_buf()]).unwrap_err();
        assert!(matches!(err, LinkError::ObjectReadFailed { .. }));
    }

    #[test]
    fn empty_input_list_reports_nothing_unprovided() {
        let report = resolve_runtime_externs(&[]).expect("no inputs is not an error");
        assert!(report.unprovided.is_empty());
    }
}
