//! Audit-surface analyses gated behind `edda lint` subflags
//! (codex `06-tooling.md` §8): the `--trust-points` listing and the
//! `--capability-safe-stdlib` discipline check. Both walk the resolved
//! package's per-module AST, mirroring the trust-hatch density lint.

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, Severity};
use edda_syntax::ast::{AttrArg, AttrLit, Attribute, EffectMember, ItemKind};

use crate::ResolvedPackage;
use crate::resolve::ResolveCx;

//            nominal catalogue (`02-modes-effects-refinements.md` §3.1). Hardcoded here because
//            `edda-resolve` is a dependency of `edda-types` (not the reverse), so the enum cannot
//            be imported without a dependency cycle; the catalogue is locked, so drift is bounded.
const CAPABILITY_NAMES: &[&str] = &[
    "Clock",
    "MonotonicClock",
    "Stdout",
    "Stderr",
    "Stdin",
    "Allocator",
    "Filesystem",
    "Network",
    "Random",
    "Executor",
    "ReadOnlyFilesystem",
    "SandboxedFilesystem",
    "LocalhostNetwork",
    "RestrictedNetwork",
    "BoundedAllocator",
    "DeterministicRandom",
    "Subprocess",
    "Debugger",
];

//            `EffectMember::Capability` shape with real capability references
//            (`02-modes-effects-refinements.md` §5). A bare row entry naming one
//            of these is an effect keyword, not an ambient capability.
const PURE_EFFECT_KEYWORDS: &[&str] = &["panic", "divergence", "cancellation", "nondet"];

//          `@unverified` / `@trust`-attributed item — the auditable "take my word for it" surface
/// Emit the `edda lint --trust-points` audit listing: one `Severity::Info`
/// diagnostic per `@unverified` / `@trust` annotation in the resolved
/// package, naming the hatched item, its hatch kind, and its `reason`.
///
/// Informational by design — listing the audit surface is not a violation,
/// so a project with trust hatches still exits `0` under `--trust-points`.
/// The locked diagnostic-class set is not extended; the listing reuses
/// [`DiagnosticClass::TrustHatchTooDense`] (the trust-hatch class) at
/// `Info` severity, mirroring how `emit_trust_budget_lints` reuses it.
pub fn emit_trust_points_listing(
    package: &ResolvedPackage,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
) {
    let mut count: usize = 0;
    for module in &package.modules {
        let entry = package.graph.module(module.id);
        let module_path = entry.canonical_path.to_owned_string(cx.interner);
        for item in &entry.ast.items {
            let Some(item_name) = item_decl_name(&item.kind, cx) else {
                continue;
            };
            for attr in &item.attributes {
                let kind = cx.interner.resolve(attr.name.name);
                if kind != "unverified" && kind != "trust" {
                    continue;
                }
                count += 1;
                let reason = attr_reason(attr, cx)
                    .map(|r| format!(": \"{r}\""))
                    .unwrap_or_default();
                diags.push(Diagnostic::new(
                    DiagnosticClass::TrustHatchTooDense,
                    Severity::Info,
                    item.span,
                    format!("trust point: `@{kind}` on `{module_path}.{item_name}`{reason}"),
                ));
            }
        }
    }
    diags.push(Diagnostic::new(
        DiagnosticClass::TrustHatchTooDense,
        Severity::Info,
        edda_span::Span::DUMMY,
        format!("trust-points audit: {count} `@unverified` / `@trust` annotation(s) in the project"),
    ));
}

//            are the only ground truth this lint reads — it is purely AST-structural, so it runs
//            after resolve without typecheck data
//          type and (2) any function effect row naming an ambient capability not backed by a parameter
/// Emit `edda lint --capability-safe-stdlib` findings: the V1.0 subset of
/// the stdlib capability discipline (codex `06-tooling.md` §8). Two
/// structural checks over the resolved package:
///
/// 1. **No shadowing** — a `type` / `spec` / `function` declaration whose
///    name equals a locked capability nominal type (`Filesystem`,
///    `Network`, …) is rejected: a stdlib symbol must never re-bind a
///    compiler-known capability name.
/// 2. **No silent effect elevation** — a function whose effect row names a
///    bare capability (`with {fs}`) that does not correspond to one of the
///    function's own parameters is claiming ambient authority it was never
///    granted.
///
/// The full alias-traced capability-laundering analysis (a narrowed
/// capability re-widened and returned/passed) is a follow-up over the typed
/// capability graph (codex ROADMAP); this V1.0 lint enforces the two
/// AST-decidable halves. Findings reuse [`DiagnosticClass::CapabilityEscalation`]
/// (the capability-discipline class) at its default `Error` severity.
pub fn emit_capability_safe_stdlib_lints(
    package: &ResolvedPackage,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &edda_diag::LintConfig,
) {
    let severity = lint_cfg.effective(DiagnosticClass::CapabilityEscalation);
    for module in &package.modules {
        let entry = package.graph.module(module.id);
        let module_path = entry.canonical_path.to_owned_string(cx.interner);
        for item in &entry.ast.items {
            if let Some(name) = item_decl_name(&item.kind, cx) {
                if CAPABILITY_NAMES.contains(&name) {
                    diags.push(
                        Diagnostic::new(
                            DiagnosticClass::CapabilityEscalation,
                            severity,
                            item.span,
                            format!(
                                "`{module_path}.{name}` shadows the locked capability nominal type `{name}` — \
                                 a stdlib declaration must not re-bind a compiler-known capability name",
                            ),
                        )
                        .with_note(
                            "rename the declaration; the capability nominal catalogue is reserved (02-modes-effects-refinements.md §3.1)",
                        ),
                    );
                }
            }
            let ItemKind::Function(fn_decl) = &item.kind else {
                continue;
            };
            let Some(effects) = &fn_decl.effects else {
                continue;
            };
            for member in &effects.members {
                let EffectMember::Capability(ident) = member else {
                    continue;
                };
                let cap = cx.interner.resolve(ident.name);
                if PURE_EFFECT_KEYWORDS.contains(&cap) {
                    continue;
                }
                let backed = fn_decl
                    .params
                    .iter()
                    .any(|p| cx.interner.resolve(p.name.name) == cap);
                if !backed {
                    let fn_name = cx.interner.resolve(fn_decl.name.name);
                    diags.push(
                        Diagnostic::new(
                            DiagnosticClass::CapabilityEscalation,
                            severity,
                            ident.span,
                            format!(
                                "`{module_path}.{fn_name}` names ambient capability `{cap}` in its effect row \
                                 with no matching parameter — a function may only exercise capabilities passed to it",
                            ),
                        )
                        .with_note(
                            "add a parameter for the capability, or remove the row entry; capabilities are never ambient (no synthesis)",
                        ),
                    );
                }
            }
        }
    }
}

//            spec declaration); `None` for imports, module decls, spec invocations, derives, and
//            module-level `let` bindings, which carry no shadowable nominal name
/// The declared name of a top-level item, resolved through `cx.interner`.
fn item_decl_name<'a>(kind: &ItemKind, cx: &ResolveCx<'a>) -> Option<&'a str> {
    let sym = match kind {
        ItemKind::Function(f) => f.name.name,
        ItemKind::TypeDecl(t) => t.name.name,
        ItemKind::Spec(s) => s.name.name,
        _ => return None,
    };
    Some(cx.interner.resolve(sym))
}

//            named-arg list; `None` when the attribute carries no `reason` arg or a non-string value
/// The `reason` string carried by a trust-hatch attribute, if present.
fn attr_reason<'a>(attr: &Attribute, cx: &ResolveCx<'a>) -> Option<&'a str> {
    for arg in &attr.args {
        let AttrArg::Named { key, value, .. } = arg else {
            continue;
        };
        if cx.interner.resolve(key.name) != "reason" {
            continue;
        }
        if let AttrArg::Lit { lit: AttrLit::Str(sym), .. } = value.as_ref() {
            return Some(cx.interner.resolve(*sym));
        }
    }
    None
}
