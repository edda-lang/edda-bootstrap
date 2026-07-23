//! Cascade-stage structmap emission — drives [`edda_structmap::emit`]
//! against the typed package and writes the resulting `index.toon`
//! files to disk.
//!
//! Invoked after typecheck under two conditions:
//! 1. `Command::Structmap` is the originating verb (always emits).
//! 2. `manifest.build.emit_structmap` is `true` (default) and the verb
//!    runs the cascade (build / check / run / test / bench / lint).

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig, Severity};
use edda_manifest::TokenBudgetEncoding;
use edda_span::Span;
use edda_structmap::{
    BudgetConfig, BudgetReport, ClusterReport, Directive, EmitInput, FileCohesionReport,
    FilenameCluster, Gate, TokenEncoding, TokenizerKind, WorklistEntry, emit,
};
use edda_types::TypedPackage;

use crate::command::StructureBudgetMode;
use crate::context::Driver;

/// Drive the structmap emission for the active driver's typed package.
/// I/O failures push a `parse_error` diagnostic onto the driver's take
/// (matching the convention `drive_codegen` uses for cache write errors)
/// but do not abort the rest of the cascade.
pub(crate) fn drive_structmap(driver: &mut Driver, typed: &TypedPackage) {
    let project_name: &str = &driver.manifest.package;
    let compiler_version = env!("CARGO_PKG_VERSION");

    let input = EmitInput {
        project_name,
        package_root: &driver.package_root,
        compiler_version,
        resolved: driver
            .resolved
            .as_ref()
            .expect("drive_structmap called before resolve completed"),
        typed,
        interner: &driver.interner,
        ty_interner: &driver.ty_interner,
        source_map: &driver.source_map,
        descendant_tree: driver.manifest.structmap.descendant_tree,
        budget_config: budget_config_from_manifest(&driver.manifest.structmap),
    };

    // `emit` runs the two-pass budget gate + atomic-module deferral
    // internally (the deferral changes the emitted `index.toon`, so it is
    // part of emission, not a separate diagnostic-only step); the result
    // carries the final post-deferral worklist of self-classifying
    // directives. `--structure-budget=off` suppresses the diagnostics but
    // not the deferral accounting (the structure map is the same either
    // way — atomic interface is always loaded on demand).
    let output = emit(&input);

    // Stash the whole-package summary so `run_workspace` can roll it into
    // the workspace aggregator's `children[]` (member totals + public
    // headline). `input` (which borrows `driver`) is no longer used past
    // the `emit` call, so the mutable borrow below is sound under NLL.
    driver.structmap_summary = Some(output.summary.clone());
    // Stash the per-directory bodies too (moved, not cloned — nothing
    // else reads `output.descendant_bodies`) so `run_workspace_in_process`
    // can inline this member's items under the workspace root when
    // `[structmap] descendant_tree` resolves `true`.
    driver.descendant_bodies = output.descendant_bodies;

    let budget_mode = driver.options.structure_budget;
    if budget_mode != StructureBudgetMode::Off {
        push_budget_diagnostics(
            &mut driver.diagnostics,
            &driver.lint_cfg,
            &output.budget,
            budget_mode,
        );
    }
    push_cluster_diagnostics(
        &mut driver.diagnostics,
        &driver.lint_cfg,
        &output.cluster_reports,
    );
    push_file_cohesion_diagnostics(
        &mut driver.diagnostics,
        &driver.lint_cfg,
        &output.file_cohesion_reports,
    );

    // `structmap --check`: compare against disk and report staleness
    // instead of writing. No `create_dir_all`, no `write` — a check run is
    // side-effect-free so CI cannot regenerate-and-falsely-pass.
    if driver.options.structmap_check {
        for (path, contents) in &output.files {
            if index_toon_is_stale(path, contents) {
                push_structmap_stale(&mut driver.diagnostics, path);
            }
        }
        return;
    }

    for (path, contents) in &output.files {
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                push_io(&mut driver.diagnostics, &driver.lint_cfg, &err, path);
                continue;
            }
        }
        if let Err(err) = write_index_toon_if_changed(path, contents) {
            push_io(&mut driver.diagnostics, &driver.lint_cfg, &err, path);
        }
    }
}

/// Whether the `index.toon` at `path` is out of sync with the structure
/// map the compiler just emitted. Schema v6 dropped the `generated_at:`
/// header, so the emitted text is fully deterministic and a plain byte
/// compare is exact (no line-filtering).
pub(crate) fn index_toon_is_stale(path: &std::path::Path, contents: &str) -> bool {
    match std::fs::read_to_string(path) {
        Ok(existing) => existing != contents,
        Err(_) => true,
    }
}

/// Report one stale / missing `index.toon` under `structmap --check`.
pub(crate) fn push_structmap_stale(diagnostics: &mut Diagnostics, path: &std::path::Path) {
    diagnostics.push(
        Diagnostic::new(
            DiagnosticClass::ParseError,
            Severity::Error,
            Span::DUMMY,
            format!(
                "structmap --check: `{}` is stale — the on-disk structure map differs from \
                 what the compiler would emit (or is missing)",
                path.display()
            ),
        )
        .with_note("run `edda structmap` to regenerate it, then commit the result."),
    );
}

/// Write `contents` to `path` unless an existing file at `path` is
/// already byte-equal to `contents`. Stops every `edda check` / `edda
/// structmap` / `edda build` from churning no-op diffs across the
/// workspace, which previously forced `/exec` rebases into manual
/// reset-and-reapply paths whenever `lib/types/`-style commonly-touched
/// directories collided with parallel agents.
pub(crate) fn write_index_toon_if_changed(
    path: &std::path::Path,
    contents: &str,
) -> std::io::Result<()> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing == contents {
            return Ok(());
        }
    }
    std::fs::write(path, contents)
}

/// Build the [`BudgetConfig`] for one package from its `[structmap]`
/// manifest block.
fn budget_config_from_manifest(sm: &edda_manifest::StructmapConfig) -> BudgetConfig {
    BudgetConfig {
        node_green_max: sm.node_green_max,
        node_amber_max: sm.node_amber_max,
        encoding: match sm.token_budget_encoding {
            TokenBudgetEncoding::O200kBase => TokenEncoding::O200kBase,
            TokenBudgetEncoding::Cl100kBase => TokenEncoding::Cl100kBase,
        },
        chars_per_token: f64::from(sm.chars_per_token_centi) / 100.0,
        model_calibration: edda_structmap::DEFAULT_MODEL_CALIBRATION,
    }
}

/// Emit `structure_map_too_dense` diagnostics for the token-budget
/// worklist. The locked diagnostic class is reused with its meaning
/// updated from line-count to token-cost — the wire/locked set is
/// untouched; only the human-facing message changes.
fn push_budget_diagnostics(
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
    report: &BudgetReport,
    mode: StructureBudgetMode,
) {
    let class = DiagnosticClass::StructureMapTooDense;
    if report.tokenizer_kind == TokenizerKind::Fallback {
        diagnostics.push(
            Diagnostic::new(
                class,
                Severity::Warn,
                Span::DUMMY,
                "structure-map token budget measured with the chars_per_token fallback \
                 (BPE tokenizer unavailable) — node/spine counts are approximate"
                    .to_string(),
            )
            .with_note(
                "build with the `bpe-tokenizer` feature (default) for exact token costs.",
            ),
        );
    }
    for entry in &report.worklist {
        // Schema v7: every directive is an executable agent action —
        // there is no compiler-resolved non-event to skip. Non-partitionable
        // files are subtracted from the projection upstream, so they never
        // reach the worklist; what remains always has a verbatim next action.
        let sev = entry_severity(entry, mode, lint_cfg, class);
        let diag = Diagnostic::new(class, sev, Span::DUMMY, budget_message(entry))
            .with_note(budget_note(entry));
        diagnostics.push(diag);
    }
}

/// Resolve the emission severity for one worklist entry under the active
/// budget mode and the entry's band.
fn entry_severity(
    entry: &WorklistEntry,
    mode: StructureBudgetMode,
    lint_cfg: &LintConfig,
    class: DiagnosticClass,
) -> Severity {
    match mode {
        StructureBudgetMode::Report => Severity::Warn,
        StructureBudgetMode::Error => match entry.band {
            edda_structmap::Band::Red => lint_cfg.effective(class),
            _ => Severity::Warn,
        },
        // `Off` is filtered out before the worklist is built.
        StructureBudgetMode::Off => Severity::Warn,
    }
}

/// Human-facing severity word for a [`Band`](edda_structmap::Band).
fn band_label(band: edda_structmap::Band) -> &'static str {
    match band {
        edda_structmap::Band::Green => "green",
        edda_structmap::Band::Amber => "amber",
        edda_structmap::Band::Red => "red",
    }
}

/// Join up to `max` items, appending `, … (N more)` when truncated.
fn join_capped(items: &[String], max: usize) -> String {
    if items.len() <= max {
        return items.join(", ");
    }
    format!(
        "{}, … ({} more)",
        items[..max].join(", "),
        items.len() - max
    )
}

/// One-line headline for a worklist entry — leads with the executable
/// directive for actionable reds.
fn budget_message(entry: &WorklistEntry) -> String {
    match entry.gate {
        Gate::PerNode => {
            let head = format!(
                "Gate A (per-node, {}): {} index.toon is ~{} Opus-4.8 tokens — {} over the \
                 {}-token ceiling",
                band_label(entry.band),
                entry.node.display(),
                entry.token_cost,
                entry.overage,
                entry.ceiling,
            );
            match directive_text(entry) {
                Some(d) => format!("{head} — {d}"),
                None => head,
            }
        }
        Gate::Spine => {
            let head = match entry.directive.as_ref() {
                Some(Directive::HoardingHub { percent, .. }) => format!(
                    "Gate B (lean-hub, {}): {} hoards {}% of its own subtree's interface \
                     (~{} Opus-4.8 tokens) — a hub must be a lean conduit (≤ ⅓), not a reservoir",
                    band_label(entry.band),
                    entry.node.display(),
                    percent,
                    entry.token_cost,
                ),
                Some(Directive::AtomicHoard { files, tokens, .. }) => format!(
                    "Gate B (atomic-hoard, {}): {} hosts {} call-disjoint non-partitionable files \
                     (~{} Opus-4.8 tokens combined) that together exceed the single-read cap — \
                     a lone cohesive file is exempt, but ≥2 independent ones co-located force one \
                     over-budget read",
                    band_label(entry.band),
                    entry.node.display(),
                    files.len(),
                    tokens,
                ),
                _ => format!(
                    "Gate B (earn-your-place, {}): {} carries no files of its own and has a single \
                     child directory — a pure conduit-of-one that neither carries nor branches \
                     (its index.toon only says \"descend to the one child\", ~{} Opus-4.8 tokens)",
                    band_label(entry.band),
                    entry.node.display(),
                    entry.token_cost,
                ),
            };
            match directive_text(entry) {
                Some(d) => format!("{head} — {d}"),
                None => head,
            }
        }
    }
}

/// The executable directive text for an entry, if any.
fn directive_text(entry: &WorklistEntry) -> Option<String> {
    match entry.directive.as_ref()? {
        Directive::WideSplit { groups, fat_files } => {
            let mut parts: Vec<String> = Vec::new();
            if groups.len() >= 2 {
                let rendered: Vec<String> = groups
                    .iter()
                    .map(|g| format!("{{{}}}", join_capped(g, 6)))
                    .collect();
                parts.push(format!(
                    "splits into {} call-disjoint groups: {} — fan into {} sibling subdirectories \
                     one level down (name each by concern)",
                    groups.len(),
                    rendered.join(", "),
                    groups.len(),
                ));
            }
            for ff in fat_files {
                let disp = ff
                    .dispatcher
                    .as_ref()
                    .map(|d| format!(" (dispatcher `{d}`)"))
                    .unwrap_or_default();
                parts.push(format!(
                    "file {} (~{} Opus-4.8 tokens) alone exceeds the ceiling → partition at \
                     {{{}}}{disp}",
                    ff.file,
                    ff.tokens,
                    join_capped(&ff.cluster, 10),
                ));
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("; "))
            }
        }
        Directive::Partition { file, cluster, dispatcher } => {
            let disp = dispatcher
                .as_ref()
                .map(|d| format!(" (dispatcher `{d}`)"))
                .unwrap_or_default();
            Some(format!(
                "partition {file} — functions {{{}}} form a cluster disjoint from the rest{disp}; \
                 split them into a sibling module",
                join_capped(cluster, 10),
            ))
        }
        Directive::HoardingHub { groups, .. } => {
            if groups.len() >= 2 {
                let rendered: Vec<String> = groups
                    .iter()
                    .map(|g| format!("{{{}}}", join_capped(g, 6)))
                    .collect();
                Some(format!(
                    "distribute its own files DOWN into child subdirectories — they form {} \
                     call-disjoint groups: {} (name each by concern)",
                    groups.len(),
                    rendered.join(", "),
                ))
            } else {
                Some(
                    "distribute its own files DOWN into a child subdirectory so the hub becomes a \
                     lean router over its descendants"
                        .to_string(),
                )
            }
        }
        Directive::Flatten => Some(
            "merge this empty wrapper into its single child — every directory must carry (≥1 file) \
             or branch (≥2 children); this does neither, so promote the child and remove the hop"
                .to_string(),
        ),
        Directive::AtomicHoard { groups, .. } => {
            let rendered: Vec<String> = groups
                .iter()
                .map(|g| format!("{{{}}}", join_capped(g, 6)))
                .collect();
            Some(format!(
                "distribute these {} call-disjoint non-partitionable files into their own leaf \
                 directories (one concern each): {} — each is irreducible on its own, so isolating \
                 them keeps every over-cap interface a single-sitting read",
                groups.len(),
                rendered.join(", "),
            ))
        }
    }
}

/// Supplementary note for a worklist entry.
fn budget_note(entry: &WorklistEntry) -> String {
    if entry.gate == Gate::Spine {
        return match entry.directive.as_ref() {
            Some(Directive::HoardingHub { .. }) => "Law 1 (lean hub) is a scale-free ratio — a hub \
                 stays green by holding a minority of its subtree, at any absolute size; no token \
                 budget is involved."
                .to_string(),
            Some(Directive::AtomicHoard { .. }) => "a non-partitionable file is exempt from the \
                 per-node cap (it has no seam to split and its own leaf would still be over-cap), \
                 so a lone one is never flagged; only ≥2 independent ones whose combined interface \
                 clears the read cap re-enter the budget here — split them apart."
                .to_string(),
            _ => "Law 2 (earn-your-place): a directory earns its place by carrying a file or \
                  branching ≥2 ways. A single-child directory that holds a file is fine (thin \
                  carrying levels keep navigation precise); only the empty wrapper is flagged."
                .to_string(),
        };
    }
    "the directive names the exact seam the compiler found — execute it verbatim.".to_string()
}

fn push_cluster_diagnostics(
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
    reports: &[ClusterReport],
) {
    let class = DiagnosticClass::FilenameEncodesHierarchy;
    let sev = lint_cfg.effective(class);
    for report in reports {
        if is_codegen_directory(&report.directory) {
            continue;
        }
        for cluster in &report.clusters {
            let diag = Diagnostic::new(
                class,
                sev,
                Span::DUMMY,
                cluster_message(&report.directory, cluster),
            )
            .with_note(cluster_help_text(cluster));
            diagnostics.push(diag);
        }
        for filename in cluster_only_underscored(report) {
            let diag = Diagnostic::new(
                class,
                sev,
                Span::DUMMY,
                format!(
                    "{}: filename `{}` contains `_` — Edda filenames use directories, not underscored stems",
                    report.directory.display(),
                    filename,
                ),
            )
            .with_note(underscore_help_text(filename));
            diagnostics.push(diag);
        }
    }
}

fn cluster_message(dir: &std::path::Path, cluster: &FilenameCluster) -> String {
    format!(
        "{}: {} files share leading token `{}` — extract into `{}/`",
        dir.display(),
        cluster.members.len(),
        cluster.leading_token,
        cluster.leading_token,
    )
}

fn cluster_help_text(cluster: &FilenameCluster) -> String {
    let mut out = format!(
        "move and rename to drop the redundant `{}` prefix:\n",
        cluster.leading_token
    );
    for member in &cluster.members {
        let stem = member
            .strip_suffix(".ea")
            .or_else(|| member.strip_suffix(".edda"))
            .unwrap_or(member.as_str());
        let suffix = stem
            .strip_prefix(&format!("{}_", cluster.leading_token))
            .unwrap_or(stem);
        let new_name = if suffix.is_empty() {
            // Cluster leader case (`queue.ea` in `queue` + `queue_error` set):
            // the file becomes the directory's index. Suggest `mod.ea` as a
            // conventional name; the model may pick differently.
            "mod.ea".to_string()
        } else {
            format!("{}.ea", suffix)
        };
        out.push_str(&format!(
            "  {} -> {}/{}\n",
            member, cluster.leading_token, new_name
        ));
    }
    out
}

/// Whether a structmap report's directory lives under a spec-instantiation
/// `codegen/` ancestor. Generated artifacts use the locked
/// `<SpecName>_<ArgName>__<12-hex>.edda` naming grammar that the
/// `filename_encodes_hierarchy` rule would flag, so we skip them — the
/// user cannot act on the rename advice for compiler output.
fn is_codegen_directory(dir: &std::path::Path) -> bool {
    dir.components().any(|c| {
        matches!(
            c,
            std::path::Component::Normal(name) if name == std::ffi::OsStr::new("codegen")
        )
    })
}

/// Underscored filenames that are NOT already covered by a cluster entry
/// — avoids emitting two diagnostics for the same file when both signals
/// fire.
fn cluster_only_underscored(report: &ClusterReport) -> Vec<&String> {
    let mut covered: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for cluster in &report.clusters {
        for member in &cluster.members {
            covered.insert(member.as_str());
        }
    }
    report
        .underscore_filenames
        .iter()
        .filter(|name| !covered.contains(name.as_str()))
        .collect()
}

fn push_file_cohesion_diagnostics(
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
    reports: &[FileCohesionReport],
) {
    let class = DiagnosticClass::FileLowCohesion;
    let sev = lint_cfg.effective(class);
    for report in reports {
        let diag = Diagnostic::new(
            class,
            sev,
            Span::DUMMY,
            file_cohesion_message(report),
        )
        .with_note(file_cohesion_help_text(report));
        diagnostics.push(diag);
    }
}

fn file_cohesion_message(report: &FileCohesionReport) -> String {
    let component_count = report.components.len();
    let sizes: Vec<String> = report
        .components
        .iter()
        .map(|c| c.len().to_string())
        .collect();
    let shape = match &report.dispatcher {
        None => "disjoint call-graph clusters".to_string(),
        Some(name) => format!("spoke clusters routed through `{}`", name),
    };
    format!(
        "{}: {} lines spread across {} {} of sizes {}",
        report.file.display(),
        report.line_count,
        component_count,
        shape,
        sizes.join("/"),
    )
}

fn file_cohesion_help_text(report: &FileCohesionReport) -> String {
    // Smallest cluster is the most actionable extraction candidate — it
    // carries the fewest connected obligations and forces the smallest
    // follow-up rewrite at the call sites.
    let smallest = report
        .components
        .iter()
        .min_by_key(|c| c.len())
        .expect("file_low_cohesion fires only when components is non-empty");
    let preview: Vec<&str> = smallest.iter().take(3).map(String::as_str).collect();
    let suffix = if smallest.len() > 3 {
        format!(", ... ({} more)", smallest.len() - 3)
    } else {
        String::new()
    };
    let lead = match &report.dispatcher {
        None => format!(
            "smallest cluster has {} functions: {}{}.",
            smallest.len(),
            preview.join(", "),
            suffix,
        ),
        Some(name) => format!(
            "`{}` is a dispatcher whose removal exposes {} spoke clusters; smallest has {} functions: {}{}.",
            name,
            report.components.len(),
            smallest.len(),
            preview.join(", "),
            suffix,
        ),
    };
    format!(
        "{} Consider extracting it into a sibling file so each file holds one concern an agent can ingest in one read.",
        lead,
    )
}

fn underscore_help_text(filename: &str) -> String {
    let stem = filename
        .strip_suffix(".ea")
        .or_else(|| filename.strip_suffix(".edda"))
        .unwrap_or(filename);
    let (head, tail) = stem.split_once('_').unwrap_or((stem, ""));
    if tail.is_empty() {
        format!("rename `{}` so its stem contains no underscore", filename)
    } else if edda_syntax::keyword_token(head).is_some() {
        format!(
            "`{}` is a reserved word and cannot be used as a directory/module-path segment — \
             move into `{}_/` and rename: {} -> {}_/{}.ea",
            head, head, filename, head, tail
        )
    } else {
        format!(
            "move into `{}/` and rename: {} -> {}/{}.ea",
            head, filename, head, tail
        )
    }
}

fn push_io(
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
    err: &std::io::Error,
    path: &std::path::Path,
) {
    let class = DiagnosticClass::ParseError;
    let sev = lint_cfg.effective(class);
    let msg = format!(
        "failed to write structure map at {}: {}",
        path.display(),
        err
    );
    diagnostics.push(Diagnostic::new(class, sev, Span::DUMMY, msg));
}
