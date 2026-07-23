//! `edda hot` — watch, rebuild, and restart a served target.
//!
//! Builds the target package, spawns the produced binary as a supervised
//! child, then watches the source tree for `.ea` / `package.toml`
//! changes. On change: debounce, rebuild; on build success kill the old
//! child and relaunch; on build failure keep the old child serving and
//! surface diagnostics. Until the
//! `std.os.fs` watch surface lands, watching uses the host `notify`
//! crate directly — sanctioned because this is toolchain code,
//! not Edda source.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, Severity};
use edda_driver::{Command, render_diagnostic};
use edda_span::Span;
use notify::{RecursiveMode, Watcher};

use crate::cli::HotArgs;
use crate::driver::{build_options, invoke};
use crate::exit::{SUCCESS, SYSTEM_ERROR};

/// Quiet period after the first relevant filesystem event before a
/// rebuild fires; further events arriving inside the window extend it.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Suffix appended to the previous executable when it is moved aside so
/// the linker can write a fresh binary while the old child still runs.
const STALE_SUFFIX: &str = "stale";

/// Run the `edda hot` supervisor loop. Returns only when the watcher
/// fails or its channel disconnects; diagnostics produced by rebuild
/// cycles are rendered immediately to stderr (the process outlives the
/// usual render-at-exit path in `main`).
pub fn run_hot(args: &HotArgs, diags: &mut Diagnostics) -> u8 {
    let manifest = resolve_manifest(args);
    if !manifest.exists() {
        emit_hot_error(
            diags,
            format!("edda hot: manifest `{}` not found", manifest.display()),
        );
        return SYSTEM_ERROR;
    }
    let package_root = parent_dir(&manifest);
    let watch_root = resolve_watch_root(args, &package_root);

    let (tx, rx) = mpsc::channel();
    let mut watcher = match notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    }) {
        Ok(w) => w,
        Err(e) => {
            emit_hot_error(diags, format!("edda hot: cannot create watcher: {e}"));
            return SYSTEM_ERROR;
        }
    };
    if let Err(e) = watcher.watch(&watch_root, RecursiveMode::Recursive) {
        emit_hot_error(
            diags,
            format!("edda hot: cannot watch `{}`: {e}", watch_root.display()),
        );
        return SYSTEM_ERROR;
    }
    eprintln!(
        "edda hot: watching `{}` (target manifest `{}`)",
        watch_root.display(),
        manifest.display()
    );

    let mut child: Option<Child> = None;
    let mut stale: Option<PathBuf> = None;
    rebuild_and_restart(args, &manifest, &package_root, &mut child, &mut stale);

    loop {
        let event = match rx.recv() {
            Ok(ev) => ev,
            Err(_) => break,
        };
        if !event_is_relevant(&event) {
            continue;
        }
        // Debounce: extend the quiet window while events keep arriving.
        while rx.recv_timeout(DEBOUNCE).is_ok() {}
        eprintln!("edda hot: change detected, rebuilding...");
        rebuild_and_restart(args, &manifest, &package_root, &mut child, &mut stale);
    }

    if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    emit_hot_error(diags, "edda hot: watcher channel disconnected".to_string());
    SYSTEM_ERROR
}

/// Resolve the manifest path the supervisor builds against.
fn resolve_manifest(args: &HotArgs) -> PathBuf {
    if let Some(p) = args.common.manifest_path.as_ref() {
        return p.clone();
    }
    match args.member.as_ref() {
        Some(member) => Path::new("lib").join(member).join("package.toml"),
        None => PathBuf::from("package.toml"),
    }
}

/// The manifest's directory, or `.` when the path has no parent component.
fn parent_dir(manifest: &Path) -> PathBuf {
    manifest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Pick the directory to watch: the workspace root (CWD) for a member
/// build, the package root otherwise.
fn resolve_watch_root(args: &HotArgs, package_root: &Path) -> PathBuf {
    if args.member.is_some() {
        PathBuf::from(".")
    } else {
        package_root.to_path_buf()
    }
}

/// `true` when the notify event touches a path the rebuild cares about.
fn event_is_relevant(event: &Result<notify::Event, notify::Error>) -> bool {
    match event {
        Ok(ev) => ev.paths.iter().any(|p| path_is_relevant(p)),
        // Watcher errors are not rebuild triggers; the loop keeps serving.
        Err(_) => false,
    }
}

/// Source-tree relevance filter for a single event path.
fn path_is_relevant(p: &Path) -> bool {
    let is_source = p.extension().and_then(|e| e.to_str()) == Some("ea")
        || p.file_name().and_then(|n| n.to_str()) == Some("package.toml");
    if !is_source {
        return false;
    }
    !p.components().any(|c| {
        c.as_os_str()
            .to_str()
            .map(|s| s.starts_with('.') && s != "." || s == "target")
            .unwrap_or(true)
    })
}

/// One supervisor cycle: move the live exe aside, rebuild, and on
/// success swap the child over to the fresh binary.
fn rebuild_and_restart(
    args: &HotArgs,
    manifest: &Path,
    package_root: &Path,
    child: &mut Option<Child>,
    stale: &mut Option<PathBuf>,
) {
    reap_exited_child(child);
    if child.is_some() {
        if let Some(exe) = find_built_exe(package_root) {
            let aside = stale_path(&exe);
            let _ = std::fs::remove_file(&aside);
            if std::fs::rename(&exe, &aside).is_ok() {
                *stale = Some(aside);
            }
        }
    }

    let mut take = Diagnostics::new();
    let mut options = build_options(&args.common, false);
    options.manifest_path = manifest.to_path_buf();
    let code = invoke(Command::Build, options, &mut take);
    for d in take.iter() {
        eprint!("{}", render_diagnostic(d));
        eprintln!();
    }
    if code != SUCCESS {
        eprintln!("edda hot: build failed — keeping previous instance running");
        return;
    }

    if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    if let Some(aside) = stale.take() {
        let _ = std::fs::remove_file(&aside);
    }
    let Some(exe) = find_built_exe(package_root) else {
        eprintln!(
            "edda hot: build succeeded but no executable was produced under \
             `{}/.edda/build/` — is the target a library member?",
            package_root.display()
        );
        return;
    };
    match std::process::Command::new(&exe).args(&args.child_args).spawn() {
        Ok(c) => {
            eprintln!("edda hot: started `{}` (pid {})", exe.display(), c.id());
            *child = Some(c);
        }
        Err(e) => {
            eprintln!("edda hot: failed to spawn `{}`: {e}", exe.display());
        }
    }
}

/// Drop the child handle if the process already exited on its own.
fn reap_exited_child(child: &mut Option<Child>) {
    let exited = match child.as_mut() {
        Some(c) => match c.try_wait() {
            Ok(Some(status)) => {
                eprintln!("edda hot: served target exited ({status}); will respawn on next rebuild");
                true
            }
            Ok(None) => false,
            Err(_) => true,
        },
        None => false,
    };
    if exited {
        *child = None;
    }
}

/// Locate the freshest linked executable under
/// `<package_root>/.edda/build/<triple>/<profile>/bin/` (the link
/// stage's locked output bucket per `build-system.md` §5).
pub(crate) fn find_built_exe(package_root: &Path) -> Option<PathBuf> {
    let build_root = package_root.join(".edda").join("build");
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for triple in read_dirs(&build_root) {
        let windows = triple
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.contains("windows"))
            .unwrap_or(false);
        for profile in read_dirs(&triple) {
            let bin = profile.join("bin");
            let Ok(entries) = std::fs::read_dir(&bin) else {
                continue;
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.is_file() || !is_launch_target(&p, windows) {
                    continue;
                }
                let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
                    continue;
                };
                if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                    best = Some((modified, p));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Whether `path` is the launchable executable for a triple bucket:
/// a `.exe` file on a Windows triple, an extensionless file elsewhere.
fn is_launch_target(path: &Path, windows: bool) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if windows => ext.eq_ignore_ascii_case("exe"),
        Some(_) => false,
        None => !windows,
    }
}

/// Subdirectories of `dir`, or empty when unreadable.
fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// The aside-name for a live executable (`server.exe` → `server.exe.stale`).
fn stale_path(exe: &Path) -> PathBuf {
    let mut name = exe.as_os_str().to_os_string();
    name.push(".");
    name.push(STALE_SUFFIX);
    PathBuf::from(name)
}

/// Push a `parse_error` diagnostic at `Span::DUMMY` for supervisor-level
/// failures (no source file is in scope).
fn emit_hot_error(diags: &mut Diagnostics, message: String) {
    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        message,
    ));
}
