//! `edda key generate` orchestration — slice F of the Mímir CLI surface.
//!
//! Mints a fresh ed25519 publisher keypair via [`edda_mimir_crypto::generate`],
//! computes its canonical fingerprint, creates the per-key directory under the
//! resolved keystore root (defaulting to [`edda_mimir_crypto::default_keystore_dir`]),
//! and writes the keypair as `priv.pem` (the PRIVATE KEY PEM produced by
//! [`edda_mimir_crypto::Keypair::to_pem`]) plus `pub.pem` (the PUBLIC KEY PEM
//! produced by [`edda_mimir_crypto::PublicKey::to_pem`]).
//!
//! The crypto crate explicitly carves the directory-creation responsibility
//! out of itself ("dir not created; slice F CLI job"); this module honours
//! that hand-off. On Unix the directory and the private key are written with
//! the conventional Cargo / SSH / GPG keystore modes (`0o700` / `0o600`); on
//! Windows the mode bits are not configurable through `std::fs`, so the
//! `#[cfg(unix)]` arms are gated and Windows takes whatever inherited ACLs
//! the user profile provides.

use std::fs::DirBuilder;
use std::path::{Path, PathBuf};

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, Severity};
use edda_mimir_crypto::{Fingerprint, default_keystore_dir, fingerprint, generate};
use edda_span::Span;

use crate::command::KeyGenerateCommand;
use crate::outcome::Outcome;

/// Execute `edda key generate`: mint a keypair and write it to the keystore.
///
/// Resolves the keystore root from `--keystore` (if supplied) or falls
/// back to [`default_keystore_dir`]. The per-key subdirectory name is
/// `--label` when present, otherwise the key's canonical fingerprint.
/// Both files are written via `std::fs::write` (Windows) or
/// permission-restricted `OpenOptions::open` (Unix).
pub fn run_generate(cmd: &KeyGenerateCommand, diags: &mut Diagnostics) -> Outcome {
    // ---- 1. Resolve the keystore root. -----------------------------------
    let keystore = match cmd.keystore.clone() {
        Some(p) => p,
        None => match default_keystore_dir() {
            Some(p) => p,
            None => {
                push_error(
                    diags,
                    "edda key generate: could not determine default keystore \
                     directory on this platform (set `--keystore <dir>` to override)"
                        .to_string(),
                );
                return Outcome::build_error(diags);
            }
        },
    };

    // ---- 2. Mint the keypair + derive its fingerprint. -------------------
    let keypair = generate();
    let pub_key = keypair.public_key();
    let fp = fingerprint(&pub_key);

    // ---- 3. Compose the per-key target directory. ------------------------
    // Fingerprints carry an `ed25519:` prefix; the literal `:` is not valid
    // in a Windows path component, so when falling back to the fingerprint
    // we replace it with `-` to keep the on-disk layout portable.
    let subdir_name = match &cmd.label {
        Some(label) => label.clone(),
        None => fp.0.replace(':', "-"),
    };
    let target_dir = keystore.join(&subdir_name);

    // ---- 4. Create the directory (recursive, 0o700 on Unix). -------------
    if let Err(e) = create_keystore_dir(&target_dir) {
        push_error(
            diags,
            format!(
                "edda key generate: failed to create keystore directory `{}`: {}",
                target_dir.display(),
                e
            ),
        );
        return Outcome::build_error(diags);
    }

    // ---- 5. Write priv.pem + pub.pem (priv is 0o600 on Unix). ------------
    let priv_path = target_dir.join("priv.pem");
    let pub_path = target_dir.join("pub.pem");

    if let Err(e) = write_private_pem(&priv_path, &keypair.to_pem()) {
        push_error(
            diags,
            format!(
                "edda key generate: failed to write `{}`: {}",
                priv_path.display(),
                e
            ),
        );
        return Outcome::build_error(diags);
    }

    if let Err(e) = std::fs::write(&pub_path, pub_key.to_pem().as_bytes()) {
        push_error(
            diags,
            format!(
                "edda key generate: failed to write `{}`: {}",
                pub_path.display(),
                e
            ),
        );
        return Outcome::build_error(diags);
    }

    // ---- 6. Report success on stdout. ------------------------------------
    report_success(&fp, &target_dir);

    Outcome::success(diags)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create the per-key directory with conventional keystore permissions.
fn create_keystore_dir(target: &Path) -> std::io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(target)
}

/// Write the private-key PEM with restrictive permissions where the platform permits.
fn write_private_pem(path: &Path, contents: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents.as_bytes())
    }
}

/// Print the canonical post-generate summary to stdout.
fn report_success(fp: &Fingerprint, target_dir: &PathBuf) {
    println!("{}", fp.0);
    println!("keystore: {}", target_dir.display());
}

/// Push a `parse_error`-classed diagnostic — matches the convention every
/// other Mímir handler uses for I/O / configuration failures.
fn push_error(diags: &mut Diagnostics, message: String) {
    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        message,
    ));
}
