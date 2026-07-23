//! TOON emitter — renders one [`DirEntry`] into the codex `schema_version = 8`
//! `index.toon` text.
//!
//! Schema v8 is adaptive (a section table is emitted only when it has
//! ≥1 row), deduped (effect rows are listed once in an `effects[]`
//! legend and referenced by id from `functions[]`), and walkable (a
//! parent's `children[]` carries each child's own counts plus a
//! public-surface headline). The header is slim: the root file carries
//! `project`/`compiler_version`/`schema_version`; every non-root file
//! carries a single `loc:` line. v7 retired the `deferred[]` table:
//! every `.ea` file is rendered in full — a non-partitionable
//! file is subtracted from the budget gate's projection, never collapsed
//! to a count. v8 added the `end` column to `types[]`/`functions[]` and
//! normalized their `line` (start) column over leading attributes,
//! so `[line, end]` is a precise per-item read range.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

use crate::EmitInput;
use crate::model::{
    ChildIndex, DirEntry, FunctionEntry, INDEX_FILENAME, InvariantEntry, ModuleEntry,
    PatternEntry, SCHEMA_VERSION, Tree, TrustEntry, TypeEntry,
};

pub(crate) fn render(input: &EmitInput, tree: &Tree) -> BTreeMap<PathBuf, String> {
    let mut files: BTreeMap<PathBuf, String> = BTreeMap::new();
    for (dir, entry) in &tree.dirs {
        let path = dir.join(INDEX_FILENAME);
        let is_root = dir == input.package_root;
        let text = if is_root && input.descendant_tree {
            render_root_descendant_tree(input, tree, entry)
        } else if is_root {
            render_root(input, entry)
        } else {
            render_non_root(input, dir, entry)
        };
        files.insert(path, text);
    }
    files
}

fn render_root(input: &EmitInput, entry: &DirEntry) -> String {
    let mut s = String::new();
    write_root_header(&mut s, input.project_name, input.compiler_version);
    s.push('\n');
    render_dir_body(&mut s, entry);
    s
}

fn render_non_root(input: &EmitInput, dir: &Path, entry: &DirEntry) -> String {
    let mut s = String::new();
    writeln!(&mut s, "loc: {}", quoted_if_needed(&rel_to_root(dir, input.package_root))).ok();
    s.push('\n');
    render_dir_body(&mut s, entry);
    s
}

fn render_root_descendant_tree(input: &EmitInput, tree: &Tree, root_entry: &DirEntry) -> String {
    let mut s = String::new();
    write_root_header(&mut s, input.project_name, input.compiler_version);
    writeln!(&mut s, "descendant_tree: true").ok();
    s.push('\n');
    // Root directory section first (no `# directory:` header — it's the
    // file's own scope).
    render_dir_body(&mut s, root_entry);
    // Then every descendant in BTreeMap order (lexical path order).
    for (dir, entry) in &tree.dirs {
        if dir == input.package_root {
            continue;
        }
        let rel = rel_to_root(dir, input.package_root);
        s.push('\n');
        writeln!(&mut s, "# directory: {}", rel).ok();
        s.push('\n');
        render_dir_body(&mut s, entry);
    }
    s
}

/// Format `dir` as a forward-slash relative path against `root`, falling
/// back to the lossy display form if the strip fails (should never
/// happen because the tree-walk ensures every dir is rooted at `root`).
fn rel_to_root(dir: &Path, root: &Path) -> String {
    match dir.strip_prefix(root) {
        Ok(r) => {
            let s = r.to_string_lossy().replace('\\', "/");
            if s.is_empty() { ".".to_string() } else { s }
        }
        Err(_) => dir.to_string_lossy().into_owned(),
    }
}

fn write_root_header(out: &mut String, project_name: &str, compiler_version: &str) {
    writeln!(out, "project: {}", quoted_if_needed(project_name)).ok();
    writeln!(out, "compiler_version: {}", quoted_if_needed(compiler_version)).ok();
    writeln!(out, "schema_version: {}", SCHEMA_VERSION).ok();
}

/// Emit every populated section table for one [`DirEntry`] — the body
/// shared between [`render_root`], [`render_non_root`], the
/// descendant-tree root, and the workspace aggregator.
fn render_dir_body(out: &mut String, entry: &DirEntry) {
    // Schema v7: no file is ever deferred. The empty exclusion set keeps
    // the `render_*` helpers' shared filter mechanism (reused by
    // [`render_file_rows`] to render one file in isolation) while emitting
    // every row here.
    let none: BTreeSet<String> = BTreeSet::new();
    let all_fns: Vec<&FunctionEntry> = entry.functions.iter().collect();
    let legend = EffectsLegend::build(&all_fns);
    render_children(out, &entry.children);
    render_modules(out, &entry.modules);
    render_types(out, &entry.types, &none);
    render_effects(out, &legend);
    render_functions(out, &entry.functions, &none, &legend);
    render_invariants(out, &entry.invariants, &none);
    render_patterns(out, &entry.patterns, &none);
    render_trust(out, &entry.trust_points, &none);
}

/// The per-file effect-row legend: distinct rows → ids. `functions[]`
/// references rows by id (the `eff`/`cone` columns) instead of repeating
/// the row text on every line, which is what keeps signature-dense nodes
/// from re-inflating once they carry effects.
struct EffectsLegend {
    rows: Vec<String>,
}

impl EffectsLegend {
    fn build(funcs: &[&FunctionEntry]) -> Self {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for f in funcs {
            if !f.declared_effects.is_empty() {
                set.insert(f.declared_effects.join(", "));
            }
            if !f.effect_cone.is_empty() && !cone_equals_declared(f) {
                set.insert(f.effect_cone.join(", "));
            }
        }
        EffectsLegend {
            rows: set.into_iter().collect(),
        }
    }

    fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn id_of(&self, row: &str) -> Option<usize> {
        self.rows.iter().position(|r| r == row)
    }
}

fn cone_equals_declared(f: &FunctionEntry) -> bool {
    let mut declared = f.declared_effects.clone();
    declared.sort();
    declared.dedup();
    declared == f.effect_cone
}

fn render_effects(out: &mut String, legend: &EffectsLegend) {
    if legend.is_empty() {
        return;
    }
    writeln!(out, "effects[{}]{{id,row}}:", legend.rows.len()).ok();
    for (i, row) in legend.rows.iter().enumerate() {
        writeln!(out, "  {},{}", i, quoted_if_needed(row)).ok();
    }
    out.push('\n');
}

fn render_children(out: &mut String, rows: &[ChildIndex]) {
    if rows.is_empty() {
        return;
    }
    writeln!(out, "children[{}]{{path,types,functions,public}}:", rows.len()).ok();
    for r in rows {
        writeln!(
            out,
            "  {},{},{},{}",
            quoted_if_needed(&r.path),
            r.types,
            r.functions,
            quoted_if_needed(&r.public),
        )
        .ok();
    }
    out.push('\n');
}

fn render_modules(out: &mut String, rows: &[ModuleEntry]) {
    if rows.is_empty() {
        return;
    }
    writeln!(out, "modules[{}]{{name,file,line,visibility}}:", rows.len()).ok();
    for r in rows {
        writeln!(
            out,
            "  {},{},{},{}",
            quoted_if_needed(&r.name),
            quoted_if_needed(&r.file),
            r.line,
            r.visibility.as_str(),
        )
        .ok();
    }
    out.push('\n');
}

fn render_types(out: &mut String, rows: &[TypeEntry], deferred: &BTreeSet<String>) {
    let visible: Vec<&TypeEntry> =
        rows.iter().filter(|r| !deferred.contains(&r.file)).collect();
    if visible.is_empty() {
        return;
    }
    writeln!(
        out,
        "types[{}]{{name,kind,file,line,end,visibility,stability,generics,fields_or_variants,refinements}}:",
        visible.len()
    )
    .ok();
    for r in visible {
        writeln!(
            out,
            "  {},{},{},{},{},{},{},{},{},{}",
            quoted_if_needed(&r.name),
            r.kind.as_str(),
            quoted_if_needed(&r.file),
            r.line,
            r.end,
            r.visibility.as_str(),
            r.stability.as_str(),
            quoted_if_needed(&join_with_pipe(&r.generics)),
            quoted_if_needed(&r.fields_or_variants),
            quoted_if_needed(&join_with_pipe(&r.refinements)),
        )
        .ok();
    }
    out.push('\n');
}

fn render_functions(
    out: &mut String,
    rows: &[FunctionEntry],
    deferred: &BTreeSet<String>,
    legend: &EffectsLegend,
) {
    let visible: Vec<&FunctionEntry> =
        rows.iter().filter(|r| !deferred.contains(&r.file)).collect();
    if visible.is_empty() {
        return;
    }
    writeln!(
        out,
        "functions[{}]{{name,file,line,end,visibility,stability,sig,eff,cone,calls}}:",
        visible.len()
    )
    .ok();
    for r in visible {
        let eff = if r.declared_effects.is_empty() {
            String::new()
        } else {
            legend
                .id_of(&r.declared_effects.join(", "))
                .map(|i| i.to_string())
                .unwrap_or_default()
        };
        let cone = if cone_equals_declared(r) {
            "=".to_string()
        } else if r.effect_cone.is_empty() {
            String::new()
        } else {
            legend
                .id_of(&r.effect_cone.join(", "))
                .map(|i| i.to_string())
                .unwrap_or_default()
        };
        writeln!(
            out,
            "  {},{},{},{},{},{},{},{},{},{}",
            quoted_if_needed(&r.qualified_name),
            quoted_if_needed(&r.file),
            r.line,
            r.end,
            r.visibility.as_str(),
            r.stability.as_str(),
            quoted_if_needed(&r.sig),
            eff,
            cone,
            quoted_if_needed(&join_with_pipe(&r.calls)),
        )
        .ok();
    }
    out.push('\n');
}

fn render_invariants(out: &mut String, rows: &[InvariantEntry], deferred: &BTreeSet<String>) {
    let visible: Vec<&InvariantEntry> =
        rows.iter().filter(|r| !deferred.contains(&r.file)).collect();
    if visible.is_empty() {
        return;
    }
    writeln!(out, "invariants[{}]{{target,file,line,rule}}:", visible.len()).ok();
    for r in visible {
        writeln!(
            out,
            "  {},{},{},{}",
            quoted_if_needed(&r.target),
            quoted_if_needed(&r.file),
            r.line,
            quoted_if_needed(&r.rule),
        )
        .ok();
    }
    out.push('\n');
}

fn render_patterns(out: &mut String, rows: &[PatternEntry], deferred: &BTreeSet<String>) {
    let visible: Vec<&PatternEntry> =
        rows.iter().filter(|r| !deferred.contains(&r.file)).collect();
    if visible.is_empty() {
        return;
    }
    writeln!(out, "patterns[{}]{{name,file,line}}:", visible.len()).ok();
    for r in visible {
        writeln!(
            out,
            "  {},{},{}",
            quoted_if_needed(&r.name),
            quoted_if_needed(&r.file),
            r.line,
        )
        .ok();
    }
    out.push('\n');
}

fn render_trust(out: &mut String, rows: &[TrustEntry], deferred: &BTreeSet<String>) {
    let visible: Vec<&TrustEntry> =
        rows.iter().filter(|r| !deferred.contains(&r.file)).collect();
    if visible.is_empty() {
        return;
    }
    writeln!(
        out,
        "trust_points[{}]{{target,kind,file,line,reason}}:",
        visible.len()
    )
    .ok();
    for r in visible {
        writeln!(
            out,
            "  {},{},{},{},{}",
            quoted_if_needed(&r.target),
            r.kind.as_str(),
            quoted_if_needed(&r.file),
            r.line,
            quoted_if_needed(&r.reason),
        )
        .ok();
    }
    out.push('\n');
}

/// Render aggregator `index.toon` files for every directory in `tree`.
/// Reuses [`render_dir_body`] / [`write_root_header`] so aggregator
/// files share the byte-for-byte schema with per-package emission.
pub(crate) fn render_aggregator_tree(
    project_name: &str,
    compiler_version: &str,
    workspace_root: &Path,
    tree: &Tree,
) -> BTreeMap<PathBuf, String> {
    let mut files: BTreeMap<PathBuf, String> = BTreeMap::new();
    for (dir, entry) in &tree.dirs {
        let path = dir.join(INDEX_FILENAME);
        let mut s = String::new();
        if dir == workspace_root {
            write_root_header(&mut s, project_name, compiler_version);
        } else {
            writeln!(&mut s, "loc: {}", quoted_if_needed(&rel_to_root(dir, workspace_root))).ok();
        }
        s.push('\n');
        render_dir_body(&mut s, entry);
        files.insert(path, s);
    }
    files
}

/// Render one [`DirEntry`]'s item-table body in isolation — no header,
/// no `loc:` line, just the adaptive section tables `render_dir_body`
/// would otherwise write directly into a growing `index.toon` string.
pub(crate) fn render_dir_body_owned(entry: &DirEntry) -> String {
    let mut s = String::new();
    render_dir_body(&mut s, entry);
    s
}

/// Render the workspace-root `index.toon` in full-descendant-tree form
/// from a pre-assembled `dir -> body text` map. `bodies` must contain an
/// entry keyed at `workspace_root` itself (that directory's own body,
/// e.g. its `children[]` router row) plus one entry per descendant
/// directory to inline, each keyed by absolute path.
pub(crate) fn render_workspace_descendant_tree(
    project_name: &str,
    compiler_version: &str,
    workspace_root: &Path,
    bodies: &BTreeMap<PathBuf, String>,
) -> String {
    let mut s = String::new();
    write_root_header(&mut s, project_name, compiler_version);
    writeln!(&mut s, "descendant_tree: true").ok();
    s.push('\n');
    if let Some(root_body) = bodies.get(workspace_root) {
        s.push_str(root_body);
    }
    for (dir, body) in bodies {
        if dir == workspace_root {
            continue;
        }
        let rel = rel_to_root(dir, workspace_root);
        s.push('\n');
        writeln!(&mut s, "# directory: {}", rel).ok();
        s.push('\n');
        s.push_str(body);
    }
    s
}

/// Render just one file's interface rows. Used by the budget gate to
/// find the dominant file in a red directory and to project the node
/// cost after deferring a file.
pub(crate) fn render_file_rows(entry: &DirEntry, file: &str) -> String {
    let others: BTreeSet<String> = entry
        .types
        .iter()
        .map(|t| t.file.clone())
        .chain(entry.functions.iter().map(|f| f.file.clone()))
        .filter(|f| f != file)
        .collect();
    let visible_fns: Vec<&FunctionEntry> = entry
        .functions
        .iter()
        .filter(|r| !others.contains(&r.file))
        .collect();
    let legend = EffectsLegend::build(&visible_fns);
    let mut s = String::new();
    render_types(&mut s, &entry.types, &others);
    render_effects(&mut s, &legend);
    render_functions(&mut s, &entry.functions, &others, &legend);
    s
}

fn join_with_pipe(items: &[String]) -> String {
    items.join(" | ")
}

fn quoted_if_needed(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_string();
    }
    let needs_quotes = s.chars().any(|c| {
        c == ','
            || c == '"'
            || c == '\n'
            || c == '\r'
            || c == '#'
            || c == '{'
            || c == '}'
            || c == '['
            || c == ']'
            || c == ':'
    }) || s.starts_with(' ')
        || s.ends_with(' ');
    if !needs_quotes {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
