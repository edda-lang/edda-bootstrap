//! `edda add` orchestration — §8.1.
//!
//! Resolves a dependency by name (optionally pinned to a version requirement),
//! fetches the `.rune` archive from the configured registry, verifies the full
//! trust chain (per-file BLAKE3s, ed25519 signature, three rollup hashes), pins
//! the resolved version into `package.toml`, and regenerates the lockfile.
//!
//! Aborts without modifying any file if verification fails.
//!
//! ## Module layout
//!
//! - [`semver`] — version-string parsing + SemVer range satisfaction.
//! - [`manifest_io`] — manifest / lockfile / publisher-key I/O helpers.

mod manifest_io;
mod semver;

pub(crate) use manifest_io::pick_registry_source;
pub(crate) use semver::semver_satisfies_pub;

use std::path::Path;

use edda_diag::Diagnostics;
use edda_manifest::lockfile::{self, Lockfile, LockfileEntry, LockfileMeta};
use edda_manifest::{Dependency, DependencySource};
use edda_mimir_archive::unpack;
use edda_mimir_crypto::{fingerprint, verify};
use edda_mimir_hash::{compute_surface_hash, compute_rune_hash};
use edda_mimir_registry::{FetchError, NetworkCap, RegistrySource, fetch_archive, fetch_index};

use crate::command::{AddCommand, MimirOptions};
use crate::outcome::Outcome;

use manifest_io::{
    load_manifest, parse_publisher_key, push_error, registry_source_to_string,
    write_dep_to_manifest,
};
use semver::{resolve_max_effects, select_version, split_name_at_version};

/// Execute `edda add`: fetch, verify, and pin a dependency.
///
/// On success the `package.toml` adjacent to `options.manifest_path` is
/// updated with the new `[[dependencies]]` entry and `package.lock.toml`
/// is regenerated. On any verification failure an appropriate diagnostic
/// is emitted and the function returns `BuildError` without modifying any
/// file.
pub fn run_add(cmd: &AddCommand, options: &MimirOptions, diags: &mut Diagnostics) -> Outcome {
    // 1. Split the name@version positional.
    let (dep_name, version_req) = split_name_at_version(&cmd.name_at_version);

    // 2. Load the manifest to find the configured registry source.
    let manifest_path = &options.manifest_path;
    let manifest = match load_manifest(manifest_path, diags) {
        Some(m) => m,
        None => return Outcome::build_error(diags),
    };

    // `package.toml`'s dependency schema carries no per-dep registry URL, so
    // the source comes from the lockfile instead: the existing entry for
    // `dep_name` if present, otherwise the first entry's source, otherwise
    // the default registry.
    let registry_source = pick_registry_source(manifest_path, &dep_name, diags);

    // 3–8. Fetch, unpack, and verify the full trust chain.
    let verified =
        match fetch_and_verify(&dep_name, &version_req, cmd.accept_unstable, &registry_source, diags) {
            Ok(v) => v,
            Err(()) => return Outcome::build_error(diags),
        };

    // 9. Pin into package.toml.
    let new_dep = Dependency {
        name: dep_name.clone().into_boxed_str(),
        version_req: verified.resolved_version.as_str().into(),
        source: DependencySource::Registry,
        surface_hash: Some(verified.computed_surface_hash.clone().into_boxed_str()),
        max_effects: resolve_max_effects(cmd),
        accept_unstable: cmd.accept_unstable,
        publisher: Some(edda_manifest::PublisherPin {
            key_fingerprint: verified.key_fingerprint.clone().into_boxed_str(),
        }),
    };

    if let Err(e) = write_dep_to_manifest(manifest_path, &manifest, new_dep) {
        push_error(
            diags,
            format!(
                "edda add: failed to write `{}`: {}",
                manifest_path.display(),
                e
            ),
        );
        return Outcome::build_error(diags);
    }

    // 10. Regenerate the lockfile.
    if write_lockfile(manifest_path, &dep_name, &registry_source, &verified, diags).is_err() {
        return Outcome::build_error(diags);
    }

    Outcome::success(diags)
}

/// Owned trust-chain verification result handed from [`fetch_and_verify`]
/// to the manifest-pin + lockfile-regeneration phases of [`run_add`].
struct VerifiedDep {
    resolved_version: String,
    computed_surface_hash: String,
    key_fingerprint: String,
    rune_hash: String,
    effect_hash: String,
    publisher_key: String,
    publisher_sig: String,
}

//   verification failure — the caller maps that to `Outcome::build_error`
//   and writes no file
/// Phases 3–8 of `edda add`: fetch the index, select a version, fetch the
/// archive, unpack + verify per-file BLAKE3s, verify the ed25519 signature,
/// recompute the two rollup hashes, and compute the publisher-key fingerprint.
fn fetch_and_verify(
    dep_name: &str,
    version_req: &Option<String>,
    accept_unstable: bool,
    registry_source: &RegistrySource,
    diags: &mut Diagnostics,
) -> Result<VerifiedDep, ()> {
    // 3. Fetch the index and select a version.
    let entries = match fetch_index(registry_source, dep_name, NetworkCap) {
        Ok(e) => e,
        Err(FetchError::NotFound) => {
            push_error(
                diags,
                format!("edda add: package `{}` not found in registry", dep_name),
            );
            return Err(());
        }
        Err(e) => {
            push_error(diags, format!("edda add: registry index error: {}", e));
            return Err(());
        }
    };

    // Pick the highest version that satisfies the version requirement.
    let selected = match select_version(&entries, version_req, accept_unstable) {
        Some(e) => e,
        None => {
            push_error(
                diags,
                format!(
                    "edda add: no version of `{}` satisfies requirement `{}`",
                    dep_name,
                    version_req.as_deref().unwrap_or("*")
                ),
            );
            return Err(());
        }
    };

    let resolved_version = &selected.version;

    // 4. Fetch the archive bytes.
    let archive_bytes = match fetch_archive(registry_source, dep_name, resolved_version, NetworkCap)
    {
        Ok(b) => b,
        Err(FetchError::NotFound) => {
            push_error(
                diags,
                format!(
                    "edda add: archive for `{}@{}` not found in registry",
                    dep_name, resolved_version
                ),
            );
            return Err(());
        }
        Err(e) => {
            push_error(diags, format!("edda add: registry fetch error: {}", e));
            return Err(());
        }
    };

    // 5. Unpack and verify per-file BLAKE3s.
    let layout = match unpack(&archive_bytes) {
        Ok(l) => l,
        Err(e) => {
            push_error(
                diags,
                format!(
                    "edda add: archive verification failed for `{}@{}`: {}",
                    dep_name, resolved_version, e
                ),
            );
            return Err(());
        }
    };

    // 5b. Reject a rune built by a different compiler major.minor (§7.3).
    // `rune_hash = f(source × compiler)`, so a foreign major.minor is a hard
    // link-time mismatch — fail closed. Patch level is intentionally not
    // checked (patch-reproducibility is the self-host suite's job).
    let consumer = edda_manifest::CompilerPin::from_compiler_version(env!("CARGO_PKG_VERSION"));
    match edda_manifest::compiler_pin::read_compiler_pin(&layout.manifest_toml) {
        Ok(Some(pin)) => match consumer {
            Some(c) if c.major == pin.major && c.minor == pin.minor => {}
            Some(c) => {
                push_error(
                    diags,
                    format!(
                        "edda add: compiler-version mismatch for `{}@{}`: rune built by `{}`, this compiler is `{}`",
                        dep_name, resolved_version, pin, c
                    ),
                );
                return Err(());
            }
            None => {}
        },
        Ok(None) => {
            push_error(
                diags,
                format!(
                    "edda add: rune `{}@{}` carries no `compiler` pin in manifest.toml (§7.3) — republish with a current toolchain",
                    dep_name, resolved_version
                ),
            );
            return Err(());
        }
        Err(e) => {
            push_error(
                diags,
                format!(
                    "edda add: cannot read compiler pin for `{}@{}`: {}",
                    dep_name, resolved_version, e
                ),
            );
            return Err(());
        }
    }

    // 6. Verify the ed25519 signature.
    let pub_key_text = String::from_utf8_lossy(&layout.publisher_key);
    let pub_key = match parse_publisher_key(&pub_key_text) {
        Ok(k) => k,
        Err(msg) => {
            push_error(
                diags,
                format!(
                    "edda add: invalid publisher key for `{}@{}`: {}",
                    dep_name, resolved_version, msg
                ),
            );
            return Err(());
        }
    };

    let sig = match edda_mimir_crypto::Signature::from_bytes(&layout.signature_bin) {
        Ok(s) => s,
        Err(e) => {
            push_error(
                diags,
                format!(
                    "edda add: malformed signature for `{}@{}`: {}",
                    dep_name, resolved_version, e
                ),
            );
            return Err(());
        }
    };

    if let Err(e) = verify(&pub_key, &layout.hashes_toon, &sig) {
        push_error(
            diags,
            format!(
                "edda add: signature verification failed for `{}@{}`: {}",
                dep_name, resolved_version, e
            ),
        );
        return Err(());
    }

    // 7. Recompute the two rollup hashes and compare to registry-claimed values.
    let computed_rune_hash = compute_rune_hash(&archive_bytes);
    if computed_rune_hash != selected.rune_hash {
        push_error(
            diags,
            format!(
                "edda add: rune_hash mismatch for `{}@{}`: expected {}, got {}",
                dep_name, resolved_version, selected.rune_hash, computed_rune_hash
            ),
        );
        return Err(());
    }

    let computed_surface_hash = match compute_surface_hash(&layout.surface) {
        Ok(h) => h,
        Err(e) => {
            push_error(
                diags,
                format!(
                    "edda add: surface_hash computation failed for `{}@{}`: {}",
                    dep_name, resolved_version, e
                ),
            );
            return Err(());
        }
    };
    if computed_surface_hash != selected.surface_hash {
        push_error(
            diags,
            format!(
                "edda add: surface_hash mismatch for `{}@{}`: expected {}, got {}",
                dep_name, resolved_version, selected.surface_hash, computed_surface_hash
            ),
        );
        return Err(());
    }

    // 8. Compute publisher key fingerprint.
    let key_fingerprint = fingerprint(&pub_key).0;

    Ok(VerifiedDep {
        resolved_version: resolved_version.clone(),
        computed_surface_hash,
        key_fingerprint,
        rune_hash: selected.rune_hash.clone(),
        effect_hash: selected.effect_hash.clone(),
        publisher_key: selected.publisher_key.clone(),
        publisher_sig: selected.publisher_sig.clone(),
    })
}

//   lockfile, replacing in place only the entry whose name matches `dep_name`
/// Final step of `edda add`: load the existing lockfile (or start fresh),
/// replace-or-append the entry for the resolved dependency, and write it
/// back next to the manifest.
fn write_lockfile(
    manifest_path: &Path,
    dep_name: &str,
    registry_source: &RegistrySource,
    verified: &VerifiedDep,
    diags: &mut Diagnostics,
) -> Result<(), ()> {
    let lockfile_path = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("package.lock.toml");

    // Load existing lockfile to preserve other entries; start fresh if absent.
    let mut existing_runes = {
        let sm = edda_span::SourceMap::new();
        let lint_cfg = edda_diag::LintConfig::new();
        match lockfile::load(&lockfile_path, &sm, diags, &lint_cfg) {
            Ok(lf) => lf.runes,
            Err(_) => Vec::new(),
        }
    };

    // Replace or append the entry for this dep.
    let entry = LockfileEntry {
        name: dep_name.to_owned(),
        version: verified.resolved_version.clone(),
        source: registry_source_to_string(registry_source),
        rune_hash: verified.rune_hash.clone(),
        surface_hash: verified.computed_surface_hash.clone(),
        effect_hash: verified.effect_hash.clone(),
        publisher_key: verified.publisher_key.clone(),
        publisher_sig: verified.publisher_sig.clone(),
        deps: Vec::new(),
    };

    if let Some(pos) = existing_runes.iter().position(|e| e.name == dep_name) {
        existing_runes[pos] = entry;
    } else {
        existing_runes.push(entry);
    }

    let new_lockfile = Lockfile {
        runes: existing_runes,
        meta: LockfileMeta { lockfile_hash: String::new() },
    };
    if let Err(e) = lockfile::write(&new_lockfile, &lockfile_path) {
        push_error(
            diags,
            format!(
                "edda add: failed to write lockfile `{}`: {}",
                lockfile_path.display(),
                e
            ),
        );
        return Err(());
    }

    Ok(())
}

//   (fetch -> unpack -> per-file BLAKE3 -> compiler pin -> ed25519 -> rollup
//   hashes) against a `RegistrySource::File` local mirror, closing the e2e
//   gap noted.
#[cfg(test)]
mod tests {
    use super::*;
    use edda_diag::Diagnostics;
    use edda_mimir_archive::{RuneLayout, build_hashes_toon, pack};
    use edda_mimir_crypto::{generate, sign};
    use edda_mimir_hash::{compute_effect_hash, compute_rune_hash, compute_surface_hash};
    use edda_mimir_registry::IndexEntry;

    /// A minimal, known-good surface file (reused verbatim from
    /// `edda-mimir-hash`'s own reproducibility fixture) so hash computation
    /// exercises no parser edge case.
    fn sample_surface() -> Vec<(String, Vec<u8>)> {
        vec![(
            "core".to_string(),
            b"schema_version: 3\nmodule: core\n\nstable_items[0]{name,signature,effect_row,refinements,doc_high,doc_med}:\n".to_vec(),
        )]
    }

    fn hex_encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    //   (rune_hash / surface_hash / effect_hash match the packed bytes, and
    //   the ed25519 signature verifies against the embedded publisher key) —
    //   the only variable under test is `pin`
    /// Build a signed, self-consistent `.rune` archive plus its registry
    /// index entry, carrying `pin` as the `manifest.toml` compiler pin.
    fn build_signed_rune(pin: edda_manifest::CompilerPin, name: &str, version: &str) -> (Vec<u8>, IndexEntry) {
        let package_toml =
            format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\nroot_namespace = \"{name}\"\n")
                .into_bytes();
        let manifest_toml = edda_manifest::compiler_pin::inject_compiler_pin(&package_toml, pin);
        let surface = sample_surface();
        let mir: Vec<(String, Vec<u8>)> = Vec::new();
        let objects: Vec<(String, String, Vec<u8>)> = Vec::new();
        let index_toon = format!("schema_version: 3\nmodule: {name}\n").into_bytes();

        let keypair = generate();
        let publisher_key = keypair.public_key().to_pem().into_bytes();
        let hashes_toon = build_hashes_toon(&manifest_toml, &surface, &mir, &objects, &index_toon);
        let signature_bin = sign(&keypair, &hashes_toon).to_bytes().to_vec();

        let layout = RuneLayout {
            manifest_toml,
            surface,
            mir,
            objects,
            index_toon,
            hashes_toon,
            signature_bin,
            publisher_key,
        };
        let archive_bytes = pack(&layout).expect("pack should succeed");

        let entry = IndexEntry {
            name: name.to_string(),
            version: version.to_string(),
            rune_hash: compute_rune_hash(&archive_bytes),
            surface_hash: compute_surface_hash(&layout.surface).expect("surface_hash"),
            effect_hash: compute_effect_hash(&layout.surface).expect("effect_hash"),
            publisher_key: String::from_utf8(layout.publisher_key.clone()).unwrap(),
            publisher_sig: format!("ed25519:{}", hex_encode(&layout.signature_bin)),
        };

        (archive_bytes, entry)
    }

    //   helpers expect: `<dir>/<name>-<version>.rune` and
    //   `<dir>/index/<2-char-prefix>/<name>.json`
    /// Write `archive_bytes` + `entry` into a fresh local-mirror directory.
    fn write_local_mirror(dir: &std::path::Path, archive_bytes: &[u8], entry: &IndexEntry) {
        std::fs::write(dir.join(format!("{}-{}.rune", entry.name, entry.version)), archive_bytes)
            .expect("write archive");
        let prefix: String = entry.name.chars().take(2).collect();
        let index_dir = dir.join("index").join(&prefix);
        std::fs::create_dir_all(&index_dir).expect("create index dir");
        std::fs::write(
            index_dir.join(format!("{}.json", entry.name)),
            serde_json::to_string(entry).expect("serialize index entry"),
        )
        .expect("write index entry");
    }

    #[test]
    fn fetch_and_verify_rejects_foreign_compiler_pin() {
        let dep_name = "foreign-pin-dep";
        let foreign_pin = edda_manifest::CompilerPin { major: 9, minor: 9 };
        let (archive_bytes, entry) = build_signed_rune(foreign_pin, dep_name, "1.0.0");

        let dir = tempfile::tempdir().expect("tempdir");
        write_local_mirror(dir.path(), &archive_bytes, &entry);

        let registry_source = RegistrySource::File(dir.path().to_path_buf());
        let mut diags = Diagnostics::new();
        let result = fetch_and_verify(dep_name, &None, false, &registry_source, &mut diags);

        assert!(result.is_err(), "a rune built by a foreign compiler major.minor must fail closed");
        assert!(
            diags.iter().any(|d| d.message.contains("compiler-version mismatch")),
            "expected a compiler-version-mismatch diagnostic, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn fetch_and_verify_accepts_matching_compiler_pin() {
        let dep_name = "matching-pin-dep";
        let consumer_pin = edda_manifest::CompilerPin::from_compiler_version(env!("CARGO_PKG_VERSION"))
            .expect("this crate's own CARGO_PKG_VERSION must parse");
        let (archive_bytes, entry) = build_signed_rune(consumer_pin, dep_name, "1.0.0");

        let dir = tempfile::tempdir().expect("tempdir");
        write_local_mirror(dir.path(), &archive_bytes, &entry);

        let registry_source = RegistrySource::File(dir.path().to_path_buf());
        let mut diags = Diagnostics::new();
        let result = fetch_and_verify(dep_name, &None, false, &registry_source, &mut diags);

        let verified = result.expect("a rune pinned to the consumer's own compiler must verify cleanly");
        assert_eq!(verified.resolved_version, "1.0.0");
        assert!(!diags.has_errors(), "no diagnostics expected on success");
    }

    //   the one being added, so `pick_registry_source` must take its
    //   fallback-to-first-entry path rather than an exact-name
    //   match — the CLI-level path this exercises was previously untested
    #[test]
    fn run_add_fetches_and_verifies_against_file_mirror_via_fallback_source() {
        let dep_name = "new-dep";
        let consumer_pin = edda_manifest::CompilerPin::from_compiler_version(env!("CARGO_PKG_VERSION"))
            .expect("this crate's own CARGO_PKG_VERSION must parse");
        let (archive_bytes, entry) = build_signed_rune(consumer_pin, dep_name, "2.1.0");

        let mirror_dir = tempfile::tempdir().expect("mirror tempdir");
        write_local_mirror(mirror_dir.path(), &archive_bytes, &entry);

        let project_dir = tempfile::tempdir().expect("project tempdir");
        let manifest_path = project_dir.path().join("package.toml");
        std::fs::write(
            &manifest_path,
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nroot_namespace = \"consumer\"\n",
        )
        .expect("write package.toml");

        let source_str = format!("file://{}", mirror_dir.path().display());
        let seed_entry = LockfileEntry {
            name: "unrelated-dep".to_string(),
            version: "1.0.0".to_string(),
            source: source_str.clone(),
            rune_hash: "blake3:seed".to_string(),
            surface_hash: "blake3:seed".to_string(),
            effect_hash: "blake3:seed".to_string(),
            publisher_key: "ed25519:seed".to_string(),
            publisher_sig: "ed25519:seed".to_string(),
            deps: Vec::new(),
        };
        let lockfile_path = project_dir.path().join("package.lock.toml");
        lockfile::write(
            &Lockfile { runes: vec![seed_entry], meta: LockfileMeta { lockfile_hash: String::new() } },
            &lockfile_path,
        )
        .expect("seed lockfile must write");

        let cmd = AddCommand {
            name_at_version: dep_name.to_string(),
            accept_unstable: false,
            max_effects: Vec::new(),
        };
        let options = MimirOptions { manifest_path: manifest_path.clone() };
        let mut diags = Diagnostics::new();

        let outcome = run_add(&cmd, &options, &mut diags);

        assert_eq!(
            outcome.exit_code,
            crate::outcome::ExitCode::Success,
            "expected success, diags: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        assert!(!diags.has_errors(), "no diagnostics expected on success");

        let manifest_text = std::fs::read_to_string(&manifest_path).expect("read package.toml back");
        assert!(manifest_text.contains(&format!("name = \"{}\"", dep_name)));
        assert!(manifest_text.contains("version = \"2.1.0\""));

        let sm = edda_span::SourceMap::new();
        let lint_cfg = edda_diag::LintConfig::new();
        let mut load_diags = Diagnostics::new();
        let written_lockfile = lockfile::load(&lockfile_path, &sm, &mut load_diags, &lint_cfg)
            .expect("written lockfile must load and verify cleanly");
        assert_eq!(written_lockfile.runes.len(), 2, "seed entry preserved + new entry appended");

        let new_entry = written_lockfile
            .runes
            .iter()
            .find(|e| e.name == dep_name)
            .expect("new dep entry present in lockfile");
        assert_eq!(
            new_entry.source, source_str,
            "registry source resolved via pick_registry_source's fallback-to-first-entry path, not the default registry"
        );
        assert_eq!(new_entry.version, "2.1.0");
        assert!(
            written_lockfile.runes.iter().any(|e| e.name == "unrelated-dep"),
            "pre-existing lockfile entry must be preserved, not dropped"
        );
    }
}
