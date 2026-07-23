//! xtask — workspace build tasks.
//!
//! Invocation: `cargo xtask <subcommand>`.
//!
//! Subcommands:
//!   build                  the canonical bootstrap-compiler build. Runs
//!                          `cargo build --release` against the whole
//!                          workspace so `edda-rt`'s staticlib ships next
//!                          to `edda.exe`; the driver's runtime probe
//!                          (`find_edda_rt_lib`) only resolves when both
//!                          land in `target/release/`. On success, also
//!                          archives the binary + runtime (renamed to
//!                          `edda-bootstrap.exe` / `edda-bootstrap_rt.lib`)
//!                          into `$EDDA_ARCHIVE_DIR` when that variable
//!                          is set, so the bootstrap compiler survives
//!                          any `target/` cleanup or `git worktree
//!                          remove`. Forward any extra argv to cargo
//!                          verbatim (e.g. `cargo xtask build --features ...`).
//!   package                build the release binary, then stage it plus
//!                          vendored `std/`/`runes/` from a monorepo
//!                          checkout into a per-platform install archive
//!                          under `target/package/`. See `xtask::package`.
//!   corpus-check           enumerate the canonical Edda corpus
//!                          (`corpus/edda-codex/examples/edlang0-compiler/`)
//!                          and report file count + total line count.
//!   parse <path>           lex + parse one `.ea` file; render diagnostics.
//!                          Exits non-zero on any parse error.
//!   parse-roundtrip <path> parse → print → parse → print one `.ea` file;
//!                          verify the printer reaches a fixed point per the
//!                          locked round-trip rule in
//!                          `docs/tooling/structural-edits.md`.
//!
//! As bootstrap stages land, this is where per-stage acceptance runners
//! live (parse round-trip, typecheck-only, end-to-end build, target matrix).

mod package;

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_span::SourceMap;
use edda_syntax::{lex, parse_file, print_file};

const CORPUS_ROOT: &str = "corpus/edda-codex/examples/edlang0-compiler";

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("build") => build_cmd(args.collect()),
        Some("package") => package::package_cmd(),
        Some("corpus-check") => corpus_check(),
        Some("parse") => match args.next() {
            Some(p) => parse_cmd(Path::new(&p)),
            None => missing_arg("parse"),
        },
        Some("parse-roundtrip") => match args.next() {
            Some(p) => parse_roundtrip_cmd(Path::new(&p)),
            None => missing_arg("parse-roundtrip"),
        },
        Some(other) => {
            eprintln!("xtask: unknown subcommand `{other}`");
            usage();
            ExitCode::from(2)
        }
        None => {
            usage();
            ExitCode::from(2)
        }
    }
}

fn usage() {
    eprintln!("usage: cargo xtask <subcommand>");
    eprintln!("subcommands:");
    eprintln!("  build [args...]            release-build the whole workspace (cargo build --release)");
    eprintln!("  package                    build + stage a per-platform install archive under target/package/");
    eprintln!("  corpus-check               enumerate the canonical Edda corpus");
    eprintln!("  parse <path>               lex + parse one .ea file");
    eprintln!("  parse-roundtrip <path>     parse → print → parse → print, verify idempotence");
}

fn missing_arg(verb: &str) -> ExitCode {
    eprintln!("xtask: `{verb}` requires a <path> argument");
    usage();
    ExitCode::from(2)
}

/// `cargo xtask build` — the canonical bootstrap-compiler build.
///
/// Shells out to `cargo build --release` (forwarding any extra argv to
/// cargo verbatim) so the whole workspace builds together. This is the
/// only invocation that reliably produces `target/release/edda.exe` and
/// `target/release/edda_rt.lib` side by side, which the driver needs at
/// link time. Use this in place of `cargo build --release -p edda-cli`;
/// the `-p` form silently omits `edda-rt` and leaves user-built Edda
/// programs unable to resolve their `__edda_*` runtime symbols.
fn build_cmd(extra: Vec<String>) -> ExitCode {
    match cargo_build_release(&extra) {
        Ok(true) => {
            archive_built_binary();
            ExitCode::SUCCESS
        }
        Ok(false) => ExitCode::from(1),
        Err(msg) => {
            eprintln!("xtask build: {msg}");
            ExitCode::from(2)
        }
    }
}

/// Shell out to `cargo build --release` from the workspace root,
/// forwarding `extra` argv verbatim. Returns `Ok(true)` on a
/// successful build, `Ok(false)` on a build failure (non-zero exit —
/// cargo's own exit code, e.g. 101, is not propagated; callers use the
/// standard 1/2 xtask dichotomy instead), and `Err` only when the
/// `cargo` process itself could not be spawned.
pub(crate) fn cargo_build_release(extra: &[String]) -> Result<bool, String> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = std::process::Command::new(&cargo);
    cmd.arg("build").arg("--release");
    cmd.args(extra);
    cmd.current_dir(workspace_root());
    match cmd.status() {
        Ok(status) => Ok(status.success()),
        Err(err) => Err(format!("failed to invoke `{}`: {err}", cargo.to_string_lossy())),
    }
}

/// Copy the freshly-built `edda.exe` + `edda_rt.lib` into
/// `$EDDA_ARCHIVE_DIR` so the bootstrap binary survives `git worktree
/// remove` and any other `target/` cleanup. Skipped entirely when
/// `EDDA_ARCHIVE_DIR` is unset or empty. The matching `edda.pdb` is
/// archived under the renamed basename (`edda-bootstrap.pdb`) when
/// present, so diagnostic builds that turn on debug info get usable
/// symbols on Windows. Silent no-op if the source artifacts are missing
/// (the `.pdb` is normal to miss — release builds default to no debug
/// info). Errors are reported to stderr but do not fail the build.
fn archive_built_binary() {
    let dest_dir = match env::var_os("EDDA_ARCHIVE_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => return,
    };
    let release = workspace_root().join("target").join("release");
    for (name, dest_name) in [
        ("edda.exe", "edda-bootstrap.exe"),
        ("edda_rt.lib", "edda-bootstrap_rt.lib"),
        ("edda.pdb", "edda-bootstrap.pdb"),
    ] {
        let src = release.join(name);
        if !src.exists() {
            continue;
        }
        let dest = dest_dir.join(dest_name);
        match std::fs::copy(&src, &dest) {
            Ok(_) => println!(
                "xtask build: archived {} → {}",
                src.display(),
                dest.display(),
            ),
            Err(err) => eprintln!(
                "xtask build: warning — could not archive {} to {}: {err}",
                src.display(),
                dest.display(),
            ),
        }
    }
}

fn corpus_check() -> ExitCode {
    let root = workspace_root().join(CORPUS_ROOT);
    if !root.exists() {
        eprintln!("xtask: corpus not present at {}", root.display());
        eprintln!("       the Edda corpus is a separate checkout; place it at that path first.");
        return ExitCode::from(2);
    }

    let mut files: Vec<PathBuf> = Vec::new();
    if let Err(err) = collect_edda(&root, &mut files) {
        eprintln!("xtask: walking {} failed: {err}", root.display());
        return ExitCode::from(2);
    }
    files.sort();

    let mut total_lines = 0usize;
    for file in &files {
        let bytes = match std::fs::read(file) {
            Ok(bytes) => bytes,
            Err(err) => {
                eprintln!("xtask: reading {} failed: {err}", file.display());
                return ExitCode::from(2);
            }
        };
        total_lines += bytes.iter().filter(|&&b| b == b'\n').count();
    }

    println!(
        "corpus: {} files, {} lines under {}",
        files.len(),
        total_lines,
        root.display(),
    );
    ExitCode::SUCCESS
}

/// Lex + parse one `.ea` file; render diagnostics; exit 1 on any
/// `Severity::Error`. Exit code 2 is reserved for IO failures.
fn parse_cmd(path: &Path) -> ExitCode {
    let src = match read_source(path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let map = SourceMap::new();
    let file_id = map.add_file(path.to_path_buf(), src.clone());
    let interner = Interner::new();
    let mut diags = Diagnostics::new();
    let cfg = LintConfig::new();
    let tokens = lex(&src, file_id, &interner, &mut diags, &cfg);
    let _ast = parse_file(&tokens, &interner, &mut diags, &cfg);
    render_diagnostics(&diags, &map);
    if diags.has_errors() {
        eprintln!(
            "parse: {} error(s) in {}",
            diags.error_count(),
            path.display()
        );
        ExitCode::from(1)
    } else {
        println!("parse: ok ({})", path.display());
        ExitCode::SUCCESS
    }
}

/// Parse → print → parse → print one `.ea` file; verify the two
/// printed forms are byte-identical (the locked round-trip rule from
/// `docs/tooling/structural-edits.md`).
fn parse_roundtrip_cmd(path: &Path) -> ExitCode {
    let src = match read_source(path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let map = SourceMap::new();
    let interner = Interner::new();
    let cfg = LintConfig::new();

    let file_id = map.add_file(path.to_path_buf(), src.clone());
    let mut diags1 = Diagnostics::new();
    let tokens1 = lex(&src, file_id, &interner, &mut diags1, &cfg);
    let ast1 = parse_file(&tokens1, &interner, &mut diags1, &cfg);
    if diags1.has_errors() {
        render_diagnostics(&diags1, &map);
        eprintln!(
            "parse-roundtrip: first parse failed in {}",
            path.display()
        );
        return ExitCode::from(1);
    }
    let printed1 = print_file(&ast1, &interner);

    let synthetic = PathBuf::from(format!("{}#printed1", path.display()));
    let file_id2 = map.add_file(synthetic, printed1.clone());
    let mut diags2 = Diagnostics::new();
    let tokens2 = lex(&printed1, file_id2, &interner, &mut diags2, &cfg);
    let ast2 = parse_file(&tokens2, &interner, &mut diags2, &cfg);
    if diags2.has_errors() {
        render_diagnostics(&diags2, &map);
        eprintln!(
            "parse-roundtrip: re-parse of printer output failed in {}",
            path.display(),
        );
        return ExitCode::from(1);
    }
    let printed2 = print_file(&ast2, &interner);

    if printed1 != printed2 {
        eprintln!(
            "parse-roundtrip: printer not idempotent for {}",
            path.display(),
        );
        eprintln!("--- first print ---\n{printed1}");
        eprintln!("--- second print ---\n{printed2}");
        return ExitCode::from(1);
    }
    println!("parse-roundtrip: ok ({})", path.display());
    ExitCode::SUCCESS
}

/// Read a `.ea` source file into a String. IO failures project to exit 2.
fn read_source(path: &Path) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|err| {
        eprintln!("xtask: reading {} failed: {err}", path.display());
        ExitCode::from(2)
    })
}

/// Minimal diagnostic renderer: file:line:col + severity + class + message.
/// The full §8 surface format lands with `edda-driver`.
fn render_diagnostics(diags: &Diagnostics, map: &SourceMap) {
    for d in diags.iter() {
        let span = d.primary.span;
        let loc = if span.is_dummy() {
            String::from("<no-span>")
        } else {
            let lc = map.byte_to_linecol(span.file, span.lo);
            let name = map.file_name(span.file);
            format!("{}:{}:{}", name.display(), lc.line, lc.col)
        };
        eprintln!(
            "{}: {}: [{}] {}",
            loc,
            d.severity.name(),
            d.class.name(),
            d.message,
        );
    }
}

fn collect_edda(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let path = entry.path();
        if ty.is_dir() {
            collect_edda(&path, out)?;
        } else if ty.is_file() && path.extension().is_some_and(|e| e == "edda") {
            out.push(path);
        }
    }
    Ok(())
}

pub(crate) fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}
