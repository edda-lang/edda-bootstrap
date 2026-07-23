//! Post-walk analyses over a populated [`crate::model::Tree`].
//!
//! Today this module hosts the effect-cone fixed-point: a per-function
//! transitive union of declared effect-row members across every
//! intra-package callee. The cone is conservative — out-of-package
//! callees (stdlib, externs, method calls) contribute nothing because
//! the structmap does not load their function tables.

use std::collections::BTreeMap;

use crate::model::{DirEntry, Tree};

/// Fill in [`crate::model::FunctionEntry::effect_cone`] for every
/// function across `tree` by transitively unioning declared effect-row
/// members along the intra-package call graph.
pub(crate) fn compute_effect_cones(tree: &mut Tree) {
    let (qnames, declared, callees) = collect_function_index(tree);
    let cones = solve_cones(&qnames, &declared, &callees);
    apply_cones(tree, &cones);
}

fn collect_function_index(
    tree: &Tree,
) -> (Vec<String>, Vec<Vec<String>>, Vec<Vec<usize>>) {
    // Pass 1: enumerate every function and record its qualified name.
    let mut qnames: Vec<String> = Vec::new();
    let mut declared: Vec<Vec<String>> = Vec::new();
    let mut raw_calls: Vec<Vec<String>> = Vec::new();
    for entry in tree.dirs.values() {
        for f in &entry.functions {
            qnames.push(f.qualified_name.clone());
            declared.push(f.declared_effects.clone());
            raw_calls.push(f.calls.clone());
        }
    }
    let qname_index: BTreeMap<&str, usize> = qnames
        .iter()
        .enumerate()
        .map(|(i, q)| (q.as_str(), i))
        .collect();

    // Pass 2: resolve each call text to an index when we can. Bare-path
    // callees are best-effort module-qualified against the caller's
    // module prefix.
    let mut callees: Vec<Vec<usize>> = Vec::with_capacity(qnames.len());
    for (i, calls) in raw_calls.iter().enumerate() {
        let caller_module = module_of(&qnames[i]);
        let mut resolved: Vec<usize> = Vec::new();
        for call in calls {
            if let Some(idx) = resolve_call(call, caller_module, &qname_index) {
                resolved.push(idx);
            }
        }
        resolved.sort();
        resolved.dedup();
        callees.push(resolved);
    }

    (qnames, declared, callees)
}

fn module_of(qualified: &str) -> &str {
    qualified.rsplit_once('.').map(|(m, _)| m).unwrap_or("")
}

fn resolve_call(call: &str, caller_module: &str, index: &BTreeMap<&str, usize>) -> Option<usize> {
    // Method-call records start with `.` — the receiver type is opaque
    // here, so we cannot resolve them. The cone is conservative.
    if call.starts_with('.') {
        return None;
    }
    if call.contains('.') {
        return index.get(call).copied();
    }
    // Bare path — try the caller's module first.
    let qualified = format!("{}.{}", caller_module, call);
    index.get(qualified.as_str()).copied()
}

fn solve_cones(
    qnames: &[String],
    declared: &[Vec<String>],
    callees: &[Vec<usize>],
) -> Vec<Vec<String>> {
    let n = qnames.len();
    let mut cones: Vec<Vec<String>> = declared.to_vec();
    for c in &mut cones {
        c.sort();
        c.dedup();
    }
    loop {
        let mut changed = false;
        for i in 0..n {
            // Collect into a fresh buffer so we don't alias `cones[i]`
            // and `cones[j]` during the union.
            let mut additions: Vec<String> = Vec::new();
            for &j in &callees[i] {
                if j == i {
                    continue;
                }
                for member in &cones[j] {
                    if cones[i].binary_search(member).is_err()
                        && additions.binary_search(member).is_err()
                    {
                        let insert_at = additions.partition_point(|s| s < member);
                        additions.insert(insert_at, member.clone());
                    }
                }
            }
            if !additions.is_empty() {
                for m in additions {
                    let pos = cones[i].partition_point(|s| s < &m);
                    cones[i].insert(pos, m);
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    cones
}

fn apply_cones(tree: &mut Tree, cones: &[Vec<String>]) {
    let mut i = 0usize;
    for entry in tree.dirs.values_mut() {
        apply_to_dir(entry, cones, &mut i);
    }
    debug_assert_eq!(
        i,
        cones.len(),
        "apply_cones walked a different number of functions than collect_function_index"
    );
}

fn apply_to_dir(entry: &mut DirEntry, cones: &[Vec<String>], i: &mut usize) {
    for f in &mut entry.functions {
        f.effect_cone = cones[*i].clone();
        *i += 1;
    }
}
