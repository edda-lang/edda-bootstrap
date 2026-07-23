//! Runner-module source synthesis and on-disk emission.
//!
//! Turns the discovered [`PropertyTarget`]s into a runnable Edda module
//! that calls each target with concrete inputs and guards every call
//! with its `ensures` predicate, then writes the module to disk.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::properties::discover::{PropertyTarget, param_marker, result_marker};
use crate::properties::strategy::generate_values;
use crate::properties::value::Value;

//   `P<i>` markers the predicate carries; mismatched arities leave
//   stale markers in the output and the resulting source is invalid
//   Edda (the lexer rejects U+0001). Callers in this module derive
//   both ends from the same [`PropertyTarget`] so the arity is by
//   construction
/// Replace the `result` and per-parameter markers in a rendered
/// predicate with concrete call-site tokens. The `result` marker maps
/// to `r_name`; the i-th parameter marker maps to `rendered_args[i]`.
fn substitute_markers(rendered: &str, r_name: &str, rendered_args: &[String]) -> String {
    let mut out = rendered.replace(&result_marker(), r_name);
    for (i, arg) in rendered_args.iter().enumerate() {
        out = out.replace(&param_marker(i), arg);
    }
    out
}

//   complete `File` containing one `module __edda_properties` line,
//   per-target imports, and one `__edda_property_main` function that
//   exercises every target
//   `strategies`, returns `None` — the runner short-circuits with a
//   "no property targets" report
//   `m<index>` import alias, and every call/binding site in that
//   target's body uses the alias rather than the raw dotted path — two
//   distinct dotted paths can share a leaf segment (e.g.
//   `resolve.build.builtins` and `resolve.cteval.builtins` both end in
//   `builtins`), and Edda's leaf-is-canonical import rule rejects two
//   unaliased imports with the same leaf
//   binding carries no type annotation — the callee's return type may
//   be a nominal type declared unqualified in its home module (e.g.
//   `LineCol`), which does not resolve bare from this synthesised
//   module; Edda's `let` infers the type from the initialiser, so the
//   binding needs none
//   property runner exercises per `language/03-verification.md` §6
/// Synthesise the property runner module's source.
///
/// The output module imports each target's parent module (aliased to
/// avoid leaf collisions), generates concrete input tuples from each
/// target's strategies, and emits one call site per tuple with a
/// post-call `if !ensures { panic }` guard. The user (or the Pass-2
/// reentry harness — follow-up) compiles and runs the module to
/// actually exercise the properties. Edda admits no comment syntax,
/// so the emitted source carries none.
pub fn synthesize_runner_source(
    targets: &[PropertyTarget],
    samples_per_target: usize,
) -> Option<String> {
    let mut runnable: Vec<&PropertyTarget> = targets
        .iter()
        .filter(|t| !t.strategies.is_empty())
        .collect();
    if runnable.is_empty() {
        return None;
    }
    runnable.sort_by(|a, b| {
        (a.module_dot_path.as_str(), a.name.as_str())
            .cmp(&(b.module_dot_path.as_str(), b.name.as_str()))
    });

    let mut out = String::new();
    out.push_str("module __edda_properties\n\n");

    // De-duplicate imports across targets (multiple property
    // functions in the same module count once), and alias each to a
    // collision-free identifier.
    let mut imports: Vec<&str> = runnable.iter().map(|t| t.module_dot_path.as_str()).collect();
    imports.sort();
    imports.dedup();
    let aliases: HashMap<&str, String> = imports
        .iter()
        .enumerate()
        .map(|(i, module)| (*module, format!("m{i}")))
        .collect();
    for module in &imports {
        out.push_str(&format!("import {module} as {alias}\n", alias = aliases[module]));
    }
    out.push('\n');

    out.push_str("public function __edda_property_main() -> () with {panic} {\n");

    let mut binding_idx: u32 = 0;
    for target in runnable {
        let alias = aliases[target.module_dot_path.as_str()].as_str();
        // Cartesian product of per-param value lists.
        let per_param: Vec<Vec<Value>> = target
            .strategies
            .iter()
            .map(|s| generate_values(s, samples_per_target))
            .collect();
        if per_param.iter().any(|v| v.is_empty()) {
            continue;
        }
        for tuple in cartesian(&per_param) {
            let rendered_args: Vec<String> =
                tuple.iter().map(|v| v.render_source()).collect();
            let args = rendered_args.join(", ");
            let r_name = format!("r_{binding_idx}");
            binding_idx = binding_idx.saturating_add(1);
            out.push_str(&format!(
                "    let {r_name} = {alias}.{func}({args})\n",
                func = target.name,
            ));
            for ensures in &target.ensures_predicates {
                // Project markers onto concrete call-site values:
                //   - `result` marker -> the `r_<idx>` binding
                //   - each `P<i>` marker -> the i-th rendered argument
                // The same substitution feeds both the runtime check
                // and the panic message so the failure reads in
                // concrete-value form (and so the message does not
                // leak `\u{1}` sentinel bytes into the source).
                let substituted = substitute_markers(ensures, &r_name, &rendered_args);
                out.push_str(&format!(
                    "    if !{pred} {{ panic \"property violation: {module}.{func}({args}) failed ensures {raw}\" }}\n",
                    pred = substituted,
                    module = target.module_dot_path,
                    func = target.name,
                    raw = substituted,
                ));
            }
        }
    }

    out.push_str("}\n");
    Some(out)
}

fn cartesian(per_param: &[Vec<Value>]) -> Vec<Vec<Value>> {
    let mut out: Vec<Vec<Value>> = vec![Vec::new()];
    for column in per_param {
        let mut next: Vec<Vec<Value>> = Vec::with_capacity(out.len() * column.len());
        for prefix in &out {
            for v in column {
                let mut row = prefix.clone();
                row.push(v.clone());
                next.push(row);
            }
        }
        out = next;
    }
    out
}

/// Write the synthesised runner source to `output_path`, creating
/// parent directories as needed. Returns the number of bytes written.
pub fn write_runner_module(output_path: &Path, source: &str) -> io::Result<usize> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output_path, source)?;
    Ok(source.len())
}

//   currently fixed at `target/edda/properties/properties.ea` under
//   the package root
/// Compute the on-disk path the runner module is written to for a
/// given package root.
pub fn runner_module_path(package_root: &Path) -> PathBuf {
    package_root
        .join("target")
        .join("edda")
        .join("properties")
        .join("properties.ea")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::properties::strategy::Strategy;

    #[test]
    fn synthesize_runner_emits_factorial_property_check() {
        // The third success criterion's literal shape: a function with
        // `requires n > 0 ensures result >= 1`. The synthesised
        // runner module imports the function's parent module, binds
        // the call's result, and asserts the ensures predicate via
        // panic. The runner is the artefact "exercised" by
        // `edda test --properties` per
        // `corpus/edda-codex/language/03-verification.md` §6.
        //
        // `ensures_predicates` carries the marker-wrapped string the
        // marker substitution projects onto the per-tuple values at
        // synthesis time. `\u{1}R\u{1}` is the postcondition-result
        // marker emitted by `render_predicate_with_markers`.
        let target = PropertyTarget {
            module_dot_path: "my_pkg".to_string(),
            name: "factorial".to_string(),
            strategies: vec![Strategy::IntRange { lo: 1, hi: 5 }],
            param_names: vec!["n".to_string()],
            ensures_predicates: vec!["(\u{1}R\u{1} >= 1)".to_string()],
        };
        let source = synthesize_runner_source(&[target], 3).expect("non-empty");
        // Must declare the synthesised module name.
        assert!(source.contains("module __edda_properties"));
        // Must import the target's parent module, aliased.
        assert!(source.contains("import my_pkg as m0"));
        // Must call the target through the alias, with at least the
        // lower bound (1) and the upper bound (5) — the
        // boundary-inclusion property of the range generator.
        assert!(source.contains("m0.factorial(1)"));
        assert!(source.contains("m0.factorial(5)"));
        // Must guard each call with the substituted ensures predicate
        // (result -> r_<idx>).
        assert!(source.contains(">= 1"));
        // The main entry point declares panic and unit return.
        assert!(source.contains("function __edda_property_main() -> () with {panic}"));
        // The sentinel byte must not survive into the emitted source —
        // markers should be fully substituted away.
        assert!(
            !source.contains('\u{1}'),
            "marker sentinel must be substituted out of generated source"
        );
        // Edda admits no comment syntax; the generated
        // source must carry none of the three comment-opening forms.
        assert!(!source.contains("//"), "no line comments admitted");
        assert!(!source.contains("/!!"), "no doc-bang comments admitted");
        assert!(!source.contains("/*"), "no block comments admitted");
    }

    #[test]
    fn synthesize_runner_substitutes_param_in_ensures_predicate() {
        // B23 — `ensures result >= x` must substitute `x` with the
        // call-site argument value. Markers carry the projection: the
        // P0 marker stands for the first parameter, the R marker for
        // `result`.
        let target = PropertyTarget {
            module_dot_path: "m".to_string(),
            name: "double_nonneg".to_string(),
            strategies: vec![Strategy::IntRange { lo: 0, hi: 3 }],
            param_names: vec!["x".to_string()],
            // Stand-in for the marker-rendered form of
            // `(result >= x)`. Match what
            // `render_predicate_with_markers` would emit.
            ensures_predicates: vec!["(\u{1}R\u{1} >= \u{1}P0\u{1})".to_string()],
        };
        let source = synthesize_runner_source(&[target], 4).expect("non-empty");
        // The runtime check substitutes both `result` (→ `r_<idx>`)
        // and the parameter `x` (→ the call-site literal). The literal
        // `x` must not appear unbound.
        assert!(source.contains("if !(r_0 >= 0)"));
        assert!(source.contains("if !(r_1 >= 3)"));
        assert!(!source.contains(">= x)"), "raw `x` must be substituted, not left as a free variable");
        assert!(
            !source.contains('\u{1}'),
            "marker sentinel must be substituted out of generated source"
        );
    }

    #[test]
    fn synthesize_runner_returns_none_when_no_targets() {
        assert!(synthesize_runner_source(&[], 3).is_none());
    }

    #[test]
    fn synthesize_runner_skips_unanalyzable_targets() {
        let target = PropertyTarget {
            module_dot_path: "m".to_string(),
            name: "f".to_string(),
            strategies: vec![], // empty == unanalysable from caller's view
            param_names: vec!["x".to_string()],
            ensures_predicates: vec!["(true)".to_string()],
        };
        assert!(synthesize_runner_source(&[target], 3).is_none());
    }

    // Two distinct dotted module paths sharing a leaf
    // segment (`resolve.build.builtins` / `resolve.cteval.builtins`,
    // the exact shape that produced `duplicate import leaf` errors)
    // must be aliased to distinct identifiers, and every call site
    // must go through the alias rather than the raw dotted path.
    #[test]
    fn synthesize_runner_aliases_imports_with_colliding_leaves() {
        let a = PropertyTarget {
            module_dot_path: "resolve.build.builtins".to_string(),
            name: "seed".to_string(),
            strategies: vec![Strategy::IntRange { lo: 0, hi: 1 }],
            param_names: vec!["x".to_string()],
            ensures_predicates: vec![],
        };
        let b = PropertyTarget {
            module_dot_path: "resolve.cteval.builtins".to_string(),
            name: "seed".to_string(),
            strategies: vec![Strategy::IntRange { lo: 0, hi: 1 }],
            param_names: vec!["x".to_string()],
            ensures_predicates: vec![],
        };
        let source = synthesize_runner_source(&[a, b], 2).expect("non-empty");
        assert!(source.contains("import resolve.build.builtins as m0"));
        assert!(source.contains("import resolve.cteval.builtins as m1"));
        assert!(source.contains("m0.seed("));
        assert!(source.contains("m1.seed("));
        assert!(
            !source.contains("resolve.build.builtins.seed(")
                && !source.contains("resolve.cteval.builtins.seed("),
            "call sites must go through the alias, not the raw dotted path"
        );
    }

    // The `let <r_name> = ...` binding must carry no
    // type annotation; a callee's return type may be a nominal type
    // declared unqualified in its home module (e.g. `LineCol`), which
    // does not resolve bare from the synthesised module.
    #[test]
    fn synthesize_runner_binding_has_no_type_annotation() {
        let target = PropertyTarget {
            module_dot_path: "span.loc.loc".to_string(),
            name: "linecol_of".to_string(),
            strategies: vec![Strategy::IntRange { lo: 1, hi: 1 }],
            param_names: vec!["n".to_string()],
            ensures_predicates: vec![],
        };
        let source = synthesize_runner_source(&[target], 1).expect("non-empty");
        assert!(source.contains("let r_0 = m0.linecol_of(1)"));
        assert!(!source.contains(": LineCol"));
    }
}
