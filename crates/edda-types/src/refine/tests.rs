//! Fast end-to-end fixtures for field-`where`-refinement propagation at
//! read sites.
//!
//! Runs the real parse → resolve → typecheck pipeline (via
//! [`edda_resolve::build_source_graph`] /
//! [`edda_resolve::build_resolved_package`] / [`crate::check_package`])
//! against tiny single-file packages written to a real temp directory
//! (module identity + `Resolutions::lookup_path` are span/file keyed,
//! so a hand-built dummy-span AST fixture cannot exercise this pass).
//! This gives a sub-second feedback loop for iterating on
//! `field_refinement_facts`, instead of the full workspace corpus
//! check the original attempt used as its only signal (see the task
//! write-up for why that loop hid the actual root cause).
//!
//! Fixture shapes:
//!
//! - [`straight_line_unrefined_counter_precondition_call_discharges`],
//!   [`mutable_mode_unrefined_counter_call_site_discharges`] — shaped
//!   like `alloc_node`/`HirFile.next_node_id`: a callee `requires`
//!   clause reads a field with NO inline refinement, and the CALLER
//!   redeclares the identical bound itself. Must discharge cleanly
//!   both before and after `field_refinement_facts` exists — the
//!   regression guards.
//! - [`cross_frame_same_named_binding_does_not_leak_unrelated_field_fact`]
//!   — a genuine PRE-EXISTING unsoundness this task's fix closes as a
//!   side effect (independent of `field_refinement_facts` itself): a
//!   plain field-projection `Path` left un-rewritten by
//!   `substitute_paths` keeps the CALLEE's own span, which resolves
//!   by `Symbol` name against the CALLER's frame — so a
//!   same-named-but-differently-typed caller local could silently
//!   supply a WRONG field fact and falsely discharge an obligation
//!   about a completely different value. `substitute_paths` now
//!   rewrites the callee's field projection onto the actual argument
//!   (mirroring the existing method-call-callee fix), so
//!   this fixture's obligation now honestly fails instead of silently
//!   (and incorrectly) passing.
//! - [`ensures_follows_from_field_where_at_read_site`] — shaped like
//!   `Duration.as_nanos`: a function returns a field read where
//!   `ensures result >= 0` follows only from that field's own
//!   `where nanos >= 0` declaration (the `clauses.rs` wiring site).
//! - [`call_site_requires_discharges_from_argument_field_where`] — a
//!   callee's `requires` reading a refined field of its own parameter
//!   discharges at the `RequiresAtCall` call site (the
//!   `call_precondition.rs` wiring site).
//! - [`coherence_preservation_discharges_from_reassignment_source_field_where`]
//!   — a `scope(coherence)` reassignment of a refined `mutable`
//!   parameter from a refined-field read suppresses the conservative
//!   `coherence_mutable_refinement_invalidated` diagnostic (the
//!   `coherence_preservation.rs` wiring site).
//! - [`call_site_requires_on_unrefined_field_of_unnamed_arg_is_parked_not_surfaced`],
//!   [`call_site_requires_on_second_of_two_same_typed_params_is_parked_not_surfaced`],
//!   [`call_site_requires_on_unrefined_counter_with_no_caller_guard_is_parked_not_surfaced`]
//!   — parking-gate probes (not acceptance/regression tests in the
//!   usual sense): confirm the `discharge_call_site` parking gate
//!   preserves the PRE-EXISTING
//!   silent-skip on a `RequiresAtCall` obligation over an UNREFINED
//!   field when the callee's own clause does not already lift in the
//!   caller's frame — i.e. closing the `substitute_paths`
//!   hazard must not newly surface a true-but-
//!   unmigrated contract gap as a build-blocking diagnostic. Two of
//!   these are shaped after real corpus findings (`recip`/`div`'s
//!   `Rational.num != 0`, and `lower_coherence_scope`/`alloc_region`'s
//!   `LowerCtx.next_region_id`) confirmed against the actual
//!   Edda-tree source to be genuine, pre-existing contract gaps with
//!   no established caller-side bound anywhere in the source — not
//!   compiler bugs, and neither caller is branching-bodied so
//!   `body_has_branching`'s existing carve-out does not
//!   apply to them; they are tracked separately for the Edda tree's
//!   solver/mir source, not fixed here. The parking gate is narrowly
//!   scoped: [`cross_frame_same_named_binding_does_not_leak_unrelated_field_fact`]
//!   confirms it does NOT reopen the collision-driven unsoundness —
//!   a callee clause that DOES already lift in the caller's frame
//!   (the dangerous case) always uses the corrected substitution, never
//!   the parked skip.
//!
//! All fail-before/pass-after acceptance cases were confirmed to FAIL
//! on the pre-fix tree (no `field_refinement_facts` mechanism at all)
//! before the fix landed, establishing the baseline the task asked for.

use std::path::{Path, PathBuf};

use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_resolve::{
    DepIndex, FsLoader, PackageLayout, ResolveCx, StdlibIndex, build_resolved_package,
    build_source_graph,
};
use edda_span::SourceMap;
use edda_target::{Arch, AbiVariant, Os, TargetCfg, TargetTriple};

use crate::ty::TyInterner;

/// Write `source` to `<tmp>/src/main.ea` under a fresh temp package
/// root and return the root directory. Module identity is
/// path-derived (no `module` override needed) — `AGENTS.md`'s
/// single-package layout is `<pkg-root>/src/*.ea`, and the `src`
/// segment strips out of the canonical module path, so the file's
/// module becomes the package's own `root_namespace` leaf.
fn write_fixture_package(dir_name: &str, source: &str) -> PathBuf {
    let root = std::env::temp_dir().join("edda_types_refine_fixture").join(dir_name);
    let src_dir = root.join("src");
    std::fs::create_dir_all(&src_dir).expect("create fixture src dir");
    std::fs::write(src_dir.join("main.ea"), source).expect("write fixture source");
    root
}

fn host_target_cfg() -> TargetCfg {
    TargetCfg::new(TargetTriple::new(Arch::X86_64, Os::Linux, AbiVariant::Gnu))
}

/// Parse, resolve, and typecheck (with `refine` discharge, since this
/// module only compiles under the `refine` feature) the single-file
/// package rooted at `root_dir`. Returns every diagnostic produced.
fn check_fixture(root_dir: &Path) -> Vec<edda_diag::Diagnostic> {
    let entry = root_dir.join("src").join("main.ea");
    let interner = Interner::new();
    let ty_interner = TyInterner::new();
    let lint_cfg = LintConfig::new();
    let source_map = SourceMap::new();
    let mut diags = Diagnostics::new();

    let root_namespace = interner.intern("fixture");
    let package_name = interner.intern("fixture");
    let layout = PackageLayout::from_namespace(root_dir.to_path_buf(), root_namespace, package_name);
    let deps = DepIndex::new();
    let stdlib = StdlibIndex::empty();

    let cx = ResolveCx {
        layout: &layout,
        deps: &deps,
        stdlib: &stdlib,
        interner: &interner,
    };

    let graph = build_source_graph(&[entry], &cx, &FsLoader, &source_map, &mut diags, &lint_cfg);
    let resolved = build_resolved_package(graph, &cx, &mut diags, &lint_cfg);

    let target_cfg = host_target_cfg();
    let _typed = crate::check_package(
        &resolved,
        &interner,
        &ty_interner,
        &lint_cfg,
        &target_cfg,
        &mut diags,
    );

    diags.into_vec()
}

fn refinement_unproven(diags: &[edda_diag::Diagnostic]) -> Vec<&edda_diag::Diagnostic> {
    diags
        .iter()
        .filter(|d| d.class == DiagnosticClass::RefinementUnproven)
        .collect()
}

fn assert_no_parse_or_resolve_errors(diags: &[edda_diag::Diagnostic], label: &str) {
    let structural: Vec<&edda_diag::Diagnostic> = diags
        .iter()
        .filter(|d| {
            d.severity == edda_diag::Severity::Error
                && d.class != DiagnosticClass::RefinementUnproven
        })
        .collect();
    assert!(
        structural.is_empty(),
        "{label}: unexpected non-refinement errors: {:#?}",
        structural
    );
}

// Regression guard: shaped exactly like the real
// `alloc_node` / `HirFile.next_node_id` family that regressed 51-fold
// in the original attempt. `Counter.next_id` carries NO inline
// refinement — `alloc_id`'s `requires c.next_id < 100` is discharged
// purely from the caller's own straight-line-body monotone-counter
// argument, same shape `call_precondition.rs`'s module doc calls out
// as the fragile "true contract gap" family
// (`file.next_node_id < u32::MAX`). This must hold both before and
// after `field_refinement_facts` exists — the walk must be a
// structural no-op for a field with no `where` clause.
#[test]
fn straight_line_unrefined_counter_precondition_call_discharges() {
    let source = r#"
module fixture.main

type Counter {
    next_id: u32
}

function alloc_id(c: mutable Counter) -> u32
    requires c.next_id < 100
{
    let raw = c.next_id
    c.next_id = raw + 1
    return raw
}

function bump(c: mutable Counter) -> u32
    requires c.next_id < 100
{
    return alloc_id(mutable c)
}
"#;
    let root = write_fixture_package("regression_guard_counter", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "regression guard");
    let unproven = refinement_unproven(&diags);
    assert!(
        unproven.is_empty(),
        "regression guard: expected zero refinement_unproven diagnostics \
         (Counter.next_id carries no inline `where`, so field_refinement_facts \
         must be a structural no-op here), got: {:#?}",
        unproven
    );
}

// Cross-frame name-collision regression guard: the callee's param is
// named `d` and typed `Plain` (a record with an UNREFINED field also
// named `nanos`); the CALLER also has a local named `d`, but of the
// unrelated refined `Duration` type. `FnRefineEnv::lookup_path`
// resolves a substituted clause's un-rewritten field-projection `Path`
// by `Symbol` name against the CALLER's `param_sorts` (not by
// frame-qualified `BindingId`), so if the fact-collection wiring ever
// resolved the callee's `d.nanos` against the caller's same-named `d`
// instead of skipping (or against the correct argument), a same-typed
// coincidence could mask it — this fixture uses DIFFERENT types for
// the two `d`s specifically so any misresolution surfaces as either a
// sort mismatch (lift failure, safely dropped) or a wrong fact, not a
// silently-correct answer by accident.
#[test]
fn cross_frame_same_named_binding_does_not_leak_unrelated_field_fact() {
    let source = r#"
module fixture.main

type Plain {
    nanos: i64
}

type Duration {
    nanos: i64 where nanos >= 0
}

function use_plain(d: Plain) -> ()
    requires d.nanos >= 0
{
}

function caller(d: Duration, p: Plain) -> () {
    use_plain(p)
}
"#;
    let root = write_fixture_package("cross_frame_name_collision", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "cross-frame name-collision guard");
    // `use_plain`'s `requires d.nanos >= 0` is a genuine, undischargeable
    // obligation here — `Plain.nanos` carries no refinement and `caller`
    // asserts nothing about `p.nanos`. This MUST still be reported: if it
    // silently discharged, that would mean the wiring leaked a fact from
    // the caller's unrelated same-named `d: Duration` local.
    let unproven = refinement_unproven(&diags);
    assert_eq!(
        unproven.len(),
        1,
        "expected exactly one genuine refinement_unproven (Plain.nanos has no `where`, so \
         `use_plain(p)`'s call-site requires is a real gap `caller` does nothing to close); \
         a different count means either a spurious extra failure or (worse) the obligation \
         was incorrectly discharged by leaking the caller's unrelated `d: Duration` local's \
         field fact across frames. got: {:#?}",
        diags.iter().filter(|d| d.class == DiagnosticClass::RefinementUnproven).collect::<Vec<_>>()
    );
}

// Acceptance case: shaped exactly like the real
// `Duration.as_nanos` / `nanos: i64 where nanos >= 0` case.
// `as_nanos`'s `ensures result >= 0` follows for free
// from `Duration.nanos`'s own inline `where` — the fix under test.
#[test]
fn ensures_follows_from_field_where_at_read_site() {
    let source = r#"
module fixture.main

type Duration {
    nanos: i64 where nanos >= 0
}

function as_nanos(d: Duration) -> i64
    ensures result >= 0
{
    return d.nanos
}
"#;
    let root = write_fixture_package("acceptance_duration_as_nanos", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "acceptance case");
    let unproven = refinement_unproven(&diags);
    assert!(
        unproven.is_empty(),
        "acceptance case: `ensures result >= 0` on `return d.nanos` should discharge \
         for free from Duration.nanos's own `where nanos >= 0` field refinement; \
         got refinement_unproven diagnostics: {:#?}",
        unproven
    );
}

// Call-site variant of the acceptance case: a callee's `requires`
// (reading a refined field of its OWN parameter) must discharge at
// the CALL SITE via `discharge_call_site`'s substituted clause — the
// riskiest of the three wiring sites, since a plain 2-segment
// field-projection `Path` (`d.nanos`, not a `Call` callee) is left
// completely unrewritten by `substitute_paths` (its inner `match` has
// no `ExprKind::Path` arm — see the doc comment on
// `edda_types::refine::mod::substitute_paths`), so the field segment's
// span is always the CALLEE's own span even after substitution.
// `field_refinement_facts` resolves that span through the CALLER's
// `FnRefineEnv` (by design — the caller is what's being discharged),
// and `FnRefineEnv::lookup_path` resolves a `Param` binding by
// `Symbol` name (not by frame-qualified identity), so this exercises
// whether that cross-frame name lookup produces a correct fact here.
//
// `use_nanos` (the callee) has NO `requires`-on-itself route the
// caller could lean on — its precondition is checked ONLY at each
// call site. `caller`'s own `d` parameter carries no `requires`
// either, so the ONLY way `use_nanos(d)`'s `requires d.nanos >= 0`
// can discharge is the RequiresAtCall obligation picking up
// `Duration.nanos`'s own field-`where` fact through the substituted
// clause — this isolates the exact mechanism the original attempt's
// three-call-site wiring targeted.
#[test]
fn call_site_requires_discharges_from_argument_field_where() {
    let source = r#"
module fixture.main

type Duration {
    nanos: i64 where nanos >= 0
}

function use_nanos(d: Duration) -> ()
    requires d.nanos >= 0
{
}

function caller(d: Duration) -> () {
    use_nanos(d)
}
"#;
    let root = write_fixture_package("acceptance_duration_call_site_requires", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "call-site requires acceptance case");
    let unproven = refinement_unproven(&diags);
    assert!(
        unproven.is_empty(),
        "call-site acceptance case: `use_nanos(d)`'s `requires d.nanos >= 0` RequiresAtCall \
         obligation should discharge for free from Duration.nanos's own `where nanos >= 0` \
         field refinement at the call site — `caller` has no other route to prove the bound; \
         got refinement_unproven diagnostics: {:#?}",
        unproven
    );
}

// Third wiring site: `coherence_preservation.rs`'s
// SMT-precise upgrade path. `bump` reassigns its `mutable i64 where d
// >= 0` parameter, inside a `scope(coherence)` region, from `other.nanos`
// — a field read whose OWN refinement (`Duration.nanos`'s `where nanos
// >= 0`) is exactly what should let Z3 prove the reassignment preserves
// `d`'s refinement, suppressing the conservative
// `coherence_mutable_refinement_invalidated` diagnostic. `other` is a
// same-function-frame local, so this exercises the safe (non-cross-
// frame) fold `try_coherence_preservation_smt` performs.
#[test]
fn coherence_preservation_discharges_from_reassignment_source_field_where() {
    let source = r#"
module fixture.main

type Duration {
    nanos: i64 where nanos >= 0
}

function bump(d: mutable i64 where d >= 0, other: Duration) -> () {
    scope(coherence) region {
        d = other.nanos
    }
}
"#;
    let root = write_fixture_package("coherence_preservation_duration", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "coherence-preservation acceptance case");
    let invalidated: Vec<&edda_diag::Diagnostic> = diags
        .iter()
        .filter(|d| d.class == DiagnosticClass::CoherenceMutableRefinementInvalidated)
        .collect();
    assert!(
        invalidated.is_empty(),
        "coherence-preservation acceptance case: `d = other.nanos` inside the `scope(coherence)` \
         region should be provably refinement-preserving via Duration.nanos's own `where nanos \
         >= 0`, suppressing the conservative coherence_mutable_refinement_invalidated diagnostic; \
         got: {:#?}",
        invalidated
    );
}

// Regression guard, `mutable`-mode variant: like
// `straight_line_unrefined_counter_precondition_call_discharges`
// except the argument is passed `mutable` (not default/`let` mode).
// The caller REDECLARES the identical bound itself
// (`requires builder.next_region_id < 4294967295`), so this must
// discharge via the caller's own `requires_context` fold regardless
// of the call-site substitution mechanics.
#[test]
fn mutable_mode_unrefined_counter_call_site_discharges() {
    let source = r#"
module fixture.main

type LowerCtx {
    next_region_id: u32
}

function alloc_region(ctx: mutable LowerCtx) -> u32
    requires ctx.next_region_id < 4294967295
{
    let raw = ctx.next_region_id
    ctx.next_region_id = raw + 1
    return raw
}

function build(builder: mutable LowerCtx) -> u32
    requires builder.next_region_id < 4294967295
{
    return alloc_region(mutable builder)
}
"#;
    let root = write_fixture_package("regression_guard_mutable_counter", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "mutable-mode regression guard");
    let unproven = refinement_unproven(&diags);
    assert!(
        unproven.is_empty(),
        "mutable-mode regression guard: expected zero refinement_unproven diagnostics \
         (LowerCtx.next_region_id carries no inline `where`, and the argument mode is \
         `mutable`, not `let`) — got: {:#?}",
        unproven
    );
}

// Coordinator-reported corpus finding #1, re-verified as a genuine
// pre-existing contract gap that must stay PARKED (not surfaced):
// shaped exactly like the REAL caller at
// `compiler/lib/mir/src/lower/walker.ea:4444` — `lower_coherence_scope`
// calls `alloc_region(mutable builder)` as the very first statement of
// its body, with NO `requires` of its own and no preceding guard
// (confirmed against the actual source: the clause list is `with
// {...}` only, and the body is 100% straight-line — no `if`/`match`/
// `loop`/`for`/`Closure`/`Spawn`/`Handle` anywhere, so it was never
// excluded by `body_has_branching`'s existing carve-out).
// `alloc_region`'s `requires ctx.next_region_id < 4294967295` is a
// monotone-counter invariant `LowerCtx.next_region_id` carries no
// `where` for — same class as the `next_node_id`/`next_def_id` family
// already parked for branching bodies. Before the
// `substitute_paths` fix, the callee's un-rewritten `ctx.next_region_id`
// silently failed to resolve in the caller's frame (`build2` below has
// no `ctx`-named binding) and the obligation was silently skipped.
// The parking gate in `discharge_call_site` deliberately preserves
// that skip here (zero refined-field facts backing the goal, and the
// callee's own unsubstituted clause does not already lift in the
// caller's frame) — surfacing it is out of scope and would
// re-block the build the same way already documented.
#[test]
fn call_site_requires_on_unrefined_counter_with_no_caller_guard_is_parked_not_surfaced() {
    let source = r#"
module fixture.main

type LowerCtx {
    next_region_id: u32
}

function alloc_region(ctx: mutable LowerCtx) -> u32
    requires ctx.next_region_id < 4294967295
{
    let raw = ctx.next_region_id
    ctx.next_region_id = raw + 1
    return raw
}

function build2(builder: mutable LowerCtx) -> u32 {
    return alloc_region(mutable builder)
}
"#;
    let root = write_fixture_package("true_gap_mutable_counter_no_guard", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "alloc_region/lower_coherence_scope parking probe");
    let unproven = refinement_unproven(&diags);
    assert!(
        unproven.is_empty(),
        "alloc_region parking probe: `alloc_region(mutable builder)`'s \
         `requires ctx.next_region_id < 4294967295` is a genuine, pre-existing contract gap \
         (LowerCtx.next_region_id carries no `where`) that must stay PARKED (matching \
         established precedent) rather than newly surfaced as a build-blocking diagnostic — \
         got: {:#?}",
        unproven
    );
}

// Coordinator-reported corpus finding #2, re-verified as a genuine
// pre-existing contract gap that must stay PARKED (not surfaced):
// shaped exactly like
// `compiler/lib/refine/src/solver/lia/simplex/core/simplex.ea:17` —
// `recip(a: Rational) -> Rational { return div(one(), a) }` where
// `div`'s signature is `div(a: Rational, b: Rational) -> Rational
// requires b.num != 0`. The obligation substitutes to `a.num != 0` in
// `recip`'s own frame. `recip` declares no `requires`, `Rational` has
// no field-level `where` on `num`, and (confirmed against the real
// Edda-tree source) fixing this properly cascades three call-site
// hops deep (`build_pivot_row` → `pivot` → `pivot_and_update` →
// `check`/`find_entering`, the last of which needs a genuinely NEW
// `ensures` connecting its pivot selection to non-zero-ness) — out of
// scope here. Before the `substitute_paths` fix,
// `b.num` stayed completely unrewritten (callee's own span), so
// `lift_predicate` tried to resolve the callee's `b` against the
// CALLER's `param_sorts`, found none (the caller's own param is named
// `a`, not `b`), and silently skipped the obligation as
// `UnresolvedPath`. The parking gate preserves that skip.
#[test]
fn call_site_requires_on_second_of_two_same_typed_params_is_parked_not_surfaced() {
    let source = r#"
module fixture.main

type Rational {
    num: i128
    den: i128
}

function one() -> Rational {
    return Rational { num: 1, den: 1 }
}

function div(a: Rational, b: Rational) -> Rational
    requires b.num != 0
{
    return Rational { num: a.num * b.den, den: a.den * b.num }
}

function recip(a: Rational) -> Rational {
    return div(one(), a)
}
"#;
    let root = write_fixture_package("true_gap_rational_div_recip", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "recip/div parking probe");
    let unproven = refinement_unproven(&diags);
    assert!(
        unproven.is_empty(),
        "recip/div parking probe: `div(one(), a)`'s `requires b.num != 0` substitutes to \
         `a.num != 0` in `recip`'s own frame — a genuine, pre-existing contract gap \
         (Rational.num carries no `where`, and the real fix cascades 3+ call-site hops) that \
         must stay PARKED rather than newly surfaced — got: {:#?}",
        unproven
    );
}

// Generic parking-gate probe (not corpus-shaped): `caller` declares NO
// `requires` at all and calls a callee whose `requires` reads an
// UNREFINED field of a DIFFERENTLY-NAMED parameter — the same shape
// as both real corpus findings above, minimized. Confirms the parking
// gate is general, not special-cased to the two specific corpus
// fixtures.
#[test]
fn call_site_requires_on_unrefined_field_of_unnamed_arg_is_parked_not_surfaced() {
    let source = r#"
module fixture.main

type Box {
    val: i64
}

function use_box(other: Box) -> ()
    requires other.val != 0
{
}

function caller(mine: Box) -> () {
    use_box(mine)
}
"#;
    let root = write_fixture_package("true_gap_unrefined_field_unrelated_names", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "generic parking-gate probe");
    let unproven = refinement_unproven(&diags);
    assert!(
        unproven.is_empty(),
        "generic parking-gate probe: `use_box(mine)`'s `requires other.val != 0` is a genuine \
         pre-existing contract gap (Box.val carries no `where`) that must stay PARKED rather \
         than newly surfaced — got: {:#?}",
        unproven
    );
}


// Acceptance: an `ensures` clause whose PREDICATE uses a
// form outside the lifter's admitted fragment (here: a function call,
// `ExprKind::Call` is NotAdmittedInPredicate) must no longer be skipped
// silently — the author believes the contract is verified when it never
// reached the solver. Expect exactly one warn-severity
// `refinement_unproven` naming the clause.
#[test]
fn out_of_fragment_ensures_emits_unverified_warn() {
    let source = r#"
module fixture.main

function helper(n: u64) -> u64 {
    return n
}

function twice(n: u64) -> u64
    ensures result == helper(n)
{
    return n
}
"#;
    let root = write_fixture_package("skipped_ensures_warn", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "skipped-ensures warn");
    let unproven = refinement_unproven(&diags);
    assert_eq!(
        unproven.len(),
        1,
        "expected exactly one refinement_unproven for the skipped ensures, got: {:#?}",
        unproven
    );
    let diag = unproven[0];
    assert_eq!(
        diag.severity,
        edda_diag::Severity::Warn,
        "the skipped-clause diagnostic must be warn-severity (the build still passes): {:#?}",
        diag
    );
    assert!(
        diag.message.contains("ensures clause 0: not verified"),
        "message must name the clause and say it was not verified: {}",
        diag.message
    );
    let notes = diag.notes.join("|");
    assert!(
        notes.contains("never reached the solver"),
        "notes must say the clause never reached the solver: {notes}"
    );
}

// `@unverified` on the function already admits every
// obligation inside it explicitly — the skipped-clause warn would be
// pure noise there, so it is suppressed.
#[test]
fn unverified_function_suppresses_skipped_ensures_warn() {
    let source = r#"
module fixture.main

function helper(n: u64) -> u64 {
    return n
}

@unverified(reason: "fixture: gap admitted explicitly")
function twice(n: u64) -> u64
    ensures result == helper(n)
{
    return n
}
"#;
    let root = write_fixture_package("skipped_ensures_unverified_silent", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "unverified suppression");
    let unproven = refinement_unproven(&diags);
    assert!(
        unproven.is_empty(),
        "@unverified admits the gap — no skipped-clause warn expected, got: {:#?}",
        unproven
    );
}

// Scope guard: an `ensures` naming `result` in a body that
// is not single-tail-shaped fails to lift with `UnresolvedPath` — the
// deliberate `result`-binding skip (see the long comment at the binding
// site), an engine capability gap rather than an author-written
// out-of-fragment predicate (246 such sites in std alone at the time
// this landed). It must stay on the silent path, NOT become a warn.
#[test]
fn branching_body_result_ensures_stays_silent() {
    let source = r#"
module fixture.main

function round_even(n: u64) -> u64
    ensures result % 2 == 0
{
    if n % 2 == 0 {
        return n
    }
    return n + 1
}
"#;
    let root = write_fixture_package("branching_result_silent_skip", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "branching-body result skip");
    let unproven = refinement_unproven(&diags);
    assert!(
        unproven.is_empty(),
        "the result-binding skip on a branching body is deliberate and must stay silent, got: {:#?}",
        unproven
    );
}

// A `requires` clause that fails to lift is a dropped
// HYPOTHESIS (completeness, not soundness) — it is excluded from the
// obligation context, and every discharge-failure diagnostic emitted
// for the function carries a note naming it (mirroring the native
// compiler's dropped-assumption note), so a spurious counterexample
// names the assumption the solver never saw.
#[test]
fn dropped_requires_is_noted_on_discharge_failure() {
    let source = r#"
module fixture.main

function pad(a: u64, label: String) -> u64
    requires label == "x"
    ensures result >= 10
{
    return a + 5
}
"#;
    let root = write_fixture_package("dropped_requires_note", source);
    let diags = check_fixture(&root);
    assert_no_parse_or_resolve_errors(&diags, "dropped-requires note");
    let unproven = refinement_unproven(&diags);
    assert_eq!(
        unproven.len(),
        1,
        "expected exactly one refinement_unproven (the falsifiable ensures), got: {:#?}",
        unproven
    );
    let diag = unproven[0];
    assert!(
        diag.is_error(),
        "the falsifiable ensures itself is a genuine error-severity failure: {:#?}",
        diag
    );
    let notes = diag.notes.join("|");
    assert!(
        notes.contains("attempted without out-of-fragment assumption: requires clause 0"),
        "the failure must carry the dropped-assumption note: {notes}"
    );
}
