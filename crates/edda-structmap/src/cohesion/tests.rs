//! Unit tests for the per-file cohesion decision rule.

    use super::*;
    use crate::model::{FunctionEntry, ModuleEntry, StabilityMarker, Tree, Visibility};
    use std::path::PathBuf;

    fn fe(qname: &str, file: &str, calls: &[&str]) -> FunctionEntry {
        FunctionEntry {
            qualified_name: qname.to_string(),
            file: file.to_string(),
            line: 1,
            end: 1,
            visibility: Visibility::Module,
            stability: StabilityMarker::Absent,
            sig: String::new(),
            calls: calls.iter().map(|s| s.to_string()).collect(),
            effect_cone: Vec::new(),
            declared_effects: Vec::new(),
        }
    }

    fn me(name: &str, file: &str) -> ModuleEntry {
        ModuleEntry {
            name: name.to_string(),
            file: file.to_string(),
            line: 1,
            visibility: Visibility::Public,
        }
    }

    #[test]
    fn cohesion_fires_when_two_components_of_five_funcs_in_a_400loc_file() {
        let mut tree = Tree { dirs: BTreeMap::new() };
        let dir = PathBuf::from("/pkg/src");
        let mut entry = crate::model::DirEntry::default();
        entry.modules.push(me("big", "big.ea"));
        // Component A (size 5) — push_* chain through helper_a.
        entry.functions.push(fe("big.helper_a", "big.ea", &[]));
        entry.functions.push(fe("big.push_one", "big.ea", &["big.helper_a"]));
        entry.functions.push(fe("big.push_two", "big.ea", &["big.helper_a"]));
        entry.functions.push(fe("big.push_three", "big.ea", &["big.helper_a"]));
        entry.functions.push(fe("big.push_four", "big.ea", &["big.helper_a"]));
        // Component B (size 5) — render_* mutual cycle.
        entry.functions.push(fe("big.render_x", "big.ea", &["big.render_y"]));
        entry.functions.push(fe("big.render_y", "big.ea", &["big.render_z"]));
        entry.functions.push(fe("big.render_z", "big.ea", &["big.render_w"]));
        entry.functions.push(fe("big.render_w", "big.ea", &["big.render_v"]));
        entry.functions.push(fe("big.render_v", "big.ea", &["big.render_x"]));
        tree.dirs.insert(dir.clone(), entry);
        let read = |p: &Path| if p == dir.join("big.ea") { Some(450) } else { None };
        let reports = analyze_file_cohesion(&tree, &read);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].file, dir.join("big.ea"));
        assert_eq!(reports[0].line_count, 450);
        assert_eq!(reports[0].components.len(), 2);
        // Step-1 path -> no dispatcher named.
        assert!(reports[0].dispatcher.is_none());
        assert!(reports[0].components.iter().any(|c| c.contains(&"big.helper_a".to_string())));
        assert!(reports[0].components.iter().any(|c| c.contains(&"big.render_x".to_string())));
    }

    #[test]
    fn cohesion_fires_when_walker_emits_bare_name_callees() {
        // Regression: walker emits bare
        // text for same-module calls (e.g. `a2`, not `big.a2`).
        let mut tree = Tree { dirs: BTreeMap::new() };
        let dir = PathBuf::from("/pkg/src");
        let mut entry = crate::model::DirEntry::default();
        entry.modules.push(me("big", "big.ea"));
        // Two disjoint 5-chains so coverage clears MIN_TOTAL_CLUSTER_COVERAGE.
        entry.functions.push(fe("big.a1", "big.ea", &["a2"]));
        entry.functions.push(fe("big.a2", "big.ea", &["a3"]));
        entry.functions.push(fe("big.a3", "big.ea", &["a4"]));
        entry.functions.push(fe("big.a4", "big.ea", &["a5"]));
        entry.functions.push(fe("big.a5", "big.ea", &[]));
        entry.functions.push(fe("big.b1", "big.ea", &["b2"]));
        entry.functions.push(fe("big.b2", "big.ea", &["b3"]));
        entry.functions.push(fe("big.b3", "big.ea", &["b4"]));
        entry.functions.push(fe("big.b4", "big.ea", &["b5"]));
        entry.functions.push(fe("big.b5", "big.ea", &[]));
        tree.dirs.insert(dir.clone(), entry);
        let read = |p: &Path| if p == dir.join("big.ea") { Some(450) } else { None };
        let reports = analyze_file_cohesion(&tree, &read);
        assert_eq!(reports.len(), 1, "lint should fire on bare-name disjoint clusters");
        assert_eq!(reports[0].components.len(), 2);
    }

    #[test]
    fn cohesion_does_not_fire_below_loc_floor() {
        let mut tree = Tree { dirs: BTreeMap::new() };
        let dir = PathBuf::from("/pkg/src");
        let mut entry = crate::model::DirEntry::default();
        entry.modules.push(me("small", "small.ea"));
        entry.functions.push(fe("small.a", "small.ea", &[]));
        entry.functions.push(fe("small.b", "small.ea", &["small.a"]));
        entry.functions.push(fe("small.c", "small.ea", &["small.a"]));
        entry.functions.push(fe("small.d", "small.ea", &["small.e"]));
        entry.functions.push(fe("small.e", "small.ea", &["small.f"]));
        entry.functions.push(fe("small.f", "small.ea", &["small.d"]));
        tree.dirs.insert(dir.clone(), entry);
        let read = |_p: &Path| Some(100);
        let reports = analyze_file_cohesion(&tree, &read);
        assert!(reports.is_empty());
    }

    #[test]
    fn cohesion_does_not_fire_for_one_connected_cluster() {
        let mut tree = Tree { dirs: BTreeMap::new() };
        let dir = PathBuf::from("/pkg/src");
        let mut entry = crate::model::DirEntry::default();
        entry.modules.push(me("ok", "ok.ea"));
        entry.functions.push(fe("ok.entry", "ok.ea", &["ok.a", "ok.b"]));
        entry.functions.push(fe("ok.a", "ok.ea", &["ok.helper"]));
        entry.functions.push(fe("ok.b", "ok.ea", &["ok.helper"]));
        entry.functions.push(fe("ok.helper", "ok.ea", &[]));
        entry.functions.push(fe("ok.c", "ok.ea", &["ok.entry"]));
        entry.functions.push(fe("ok.d", "ok.ea", &["ok.entry"]));
        tree.dirs.insert(dir.clone(), entry);
        let read = |_p: &Path| Some(900);
        let reports = analyze_file_cohesion(&tree, &read);
        assert!(reports.is_empty());
    }

    #[test]
    fn cohesion_skips_singleton_components() {
        let mut tree = Tree { dirs: BTreeMap::new() };
        let dir = PathBuf::from("/pkg/src");
        let mut entry = crate::model::DirEntry::default();
        entry.modules.push(me("mixed", "mixed.ea"));
        entry.functions.push(fe("mixed.a", "mixed.ea", &["mixed.b"]));
        entry.functions.push(fe("mixed.b", "mixed.ea", &["mixed.c"]));
        entry.functions.push(fe("mixed.c", "mixed.ea", &[]));
        entry.functions.push(fe("mixed.lonely1", "mixed.ea", &[]));
        entry.functions.push(fe("mixed.lonely2", "mixed.ea", &[]));
        tree.dirs.insert(dir.clone(), entry);
        let read = |_p: &Path| Some(500);
        let reports = analyze_file_cohesion(&tree, &read);
        assert!(reports.is_empty());
    }

    #[test]
    fn cohesion_ignores_method_calls_and_cross_file_callees() {
        let mut tree = Tree { dirs: BTreeMap::new() };
        let dir = PathBuf::from("/pkg/src");
        let mut entry = crate::model::DirEntry::default();
        entry.modules.push(me("file1", "file1.ea"));
        // Cluster A (size 5) — only inter-cluster edges are method calls
        // / cross-file callees, which are silently dropped.
        entry.functions.push(fe(
            "file1.alpha",
            "file1.ea",
            &[".some_method", "file2.bar"],
        ));
        entry.functions.push(fe("file1.beta", "file1.ea", &["file1.alpha"]));
        entry.functions.push(fe("file1.gamma", "file1.ea", &["file1.beta"]));
        entry.functions.push(fe("file1.alpha2", "file1.ea", &["file1.gamma"]));
        entry.functions.push(fe("file1.alpha3", "file1.ea", &["file1.alpha2"]));
        // Cluster B (size 5).
        entry.functions.push(fe("file1.delta", "file1.ea", &["file1.epsilon"]));
        entry.functions.push(fe("file1.epsilon", "file1.ea", &["file1.zeta"]));
        entry.functions.push(fe("file1.zeta", "file1.ea", &["file1.delta"]));
        entry.functions.push(fe("file1.delta2", "file1.ea", &["file1.zeta"]));
        entry.functions.push(fe("file1.delta3", "file1.ea", &["file1.delta2"]));
        tree.dirs.insert(dir.clone(), entry);
        let read = |_p: &Path| Some(500);
        let reports = analyze_file_cohesion(&tree, &read);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].components.len(), 2);
    }

    #[test]
    fn cohesion_vetoes_mutual_recursion_core() {
        // Models hir/lower.ea: lower_block ↔ lower_stmt ↔ lower_expr ↔
        // lower_arm form one big SCC covering the file. A few singleton
        // helpers (synth_*) bring n above the function-count gate but
        // SCC dominance is above the 30% threshold so step 2 vetoes.
        let mut tree = Tree { dirs: BTreeMap::new() };
        let dir = PathBuf::from("/pkg/src");
        let mut entry = crate::model::DirEntry::default();
        entry.modules.push(me("lower", "lower.ea"));
        // 5-node SCC: each lower_X calls the next, last calls back to first.
        entry.functions.push(fe("lower.lower_block", "lower.ea", &["lower.lower_stmt"]));
        entry.functions.push(fe("lower.lower_stmt", "lower.ea", &["lower.lower_expr"]));
        entry.functions.push(fe("lower.lower_expr", "lower.ea", &["lower.lower_arm", "lower.lower_block"]));
        entry.functions.push(fe("lower.lower_arm", "lower.ea", &["lower.lower_expr"]));
        entry.functions.push(fe("lower.lower_for_kind", "lower.ea", &["lower.lower_block"]));
        // 3 isolated synth helpers — pushes function count to 8 so step 2 runs.
        entry.functions.push(fe("lower.synth_unit", "lower.ea", &[]));
        entry.functions.push(fe("lower.synth_arm", "lower.ea", &[]));
        entry.functions.push(fe("lower.synth_use", "lower.ea", &[]));
        tree.dirs.insert(dir.clone(), entry);
        let read = |_p: &Path| Some(900);
        let reports = analyze_file_cohesion(&tree, &read);
        // SCC is {lower_block, lower_stmt, lower_expr, lower_arm, lower_for_kind} = 5/8 = 62.5% ≥ 30%.
        // Step 2 must veto regardless of the synth-helper singletons.
        assert!(reports.is_empty(), "mutual recursion core should suppress the lint");
    }

    #[test]
    fn cohesion_vetoes_large_scc_diluted_below_ratio_threshold() {
        // Models mir/lower/walker.ea: a stable
        // 20-node recursive core diluted by 50 leaf helpers — ratio 20/70 =
        // 28.6% < 30%, so the ratio-only veto would fall through to step 3 and
        // flag it. The absolute SCC floor (>= 20) must veto regardless of ratio.
        let mut tree = Tree { dirs: BTreeMap::new() };
        let dir = PathBuf::from("/pkg/src");
        let mut entry = crate::model::DirEntry::default();
        entry.modules.push(me("walk", "walk.ea"));
        // 20-node SCC ring: each calls the next, last calls back to the first.
        let ring: Vec<String> = (0..20).map(|i| format!("walk.step_{i:02}")).collect();
        for i in 0..20 {
            let callee = ring[(i + 1) % 20].clone();
            entry.functions.push(fe(&ring[i], "walk.ea", &[callee.as_str()]));
        }
        // 50 isolated leaf helpers — dilute n to 70 (SCC ratio 28.6% < 30%).
        for i in 0..50 {
            entry.functions.push(fe(&format!("walk.leaf_{i:02}"), "walk.ea", &[]));
        }
        tree.dirs.insert(dir.clone(), entry);
        let read = |_p: &Path| Some(2000);
        let reports = analyze_file_cohesion(&tree, &read);
        assert!(
            reports.is_empty(),
            "a 20-fn recursive core is atomic even when leaf helpers dilute the ratio below 30%",
        );
    }

    #[test]
    fn cohesion_fires_on_hub_and_spoke_after_dispatcher_removal() {
        // Models emit.ea: render_dir_body dispatches to 5 spoke pairs
        // (total cluster coverage = 10, clears MIN_TOTAL_CLUSTER_COVERAGE).
        // No mutual recursion (each render_X calls its count_X leaf;
        // count_X has no outgoing edges).
        let mut tree = Tree { dirs: BTreeMap::new() };
        let dir = PathBuf::from("/pkg/src");
        let mut entry = crate::model::DirEntry::default();
        entry.modules.push(me("emit", "emit.ea"));
        entry.functions.push(fe(
            "emit.render_dir_body",
            "emit.ea",
            &[
                "emit.render_modules",
                "emit.render_types",
                "emit.render_fns",
                "emit.render_trust",
                "emit.render_docs",
            ],
        ));
        entry.functions.push(fe("emit.render_modules", "emit.ea", &["emit.count_modules"]));
        entry.functions.push(fe("emit.count_modules", "emit.ea", &[]));
        entry.functions.push(fe("emit.render_types", "emit.ea", &["emit.count_types"]));
        entry.functions.push(fe("emit.count_types", "emit.ea", &[]));
        entry.functions.push(fe("emit.render_fns", "emit.ea", &["emit.count_fns"]));
        entry.functions.push(fe("emit.count_fns", "emit.ea", &[]));
        entry.functions.push(fe("emit.render_trust", "emit.ea", &["emit.count_trust"]));
        entry.functions.push(fe("emit.count_trust", "emit.ea", &[]));
        entry.functions.push(fe("emit.render_docs", "emit.ea", &["emit.count_docs"]));
        entry.functions.push(fe("emit.count_docs", "emit.ea", &[]));
        tree.dirs.insert(dir.clone(), entry);
        let read = |_p: &Path| Some(700);
        let reports = analyze_file_cohesion(&tree, &read);
        assert_eq!(reports.len(), 1, "hub-and-spoke pattern should fire");
        let r = &reports[0];
        assert_eq!(r.dispatcher.as_deref(), Some("emit.render_dir_body"));
        // 5 spoke pairs of size 2 each (total = 10).
        assert_eq!(r.components.len(), 5);
        for c in &r.components {
            assert_eq!(c.len(), 2);
        }
    }

    #[test]
    fn cohesion_terminates_on_large_hub_and_spoke() {
        // Regression: the previous
        // `spoke_decomposition` materialised one `Vec<bool>` per spoke
        // simultaneously, giving O(V × out_degree(dispatcher)) peak
        // memory. With a wide hub (50 spokes) and many functions (201)
        // the old code allocated a 50 × 201 boolean matrix per spoke
        // and repeated that pattern for every candidate dispatcher in
        // the file. The rewritten version uses one reusable Vec<bool>
        // plus a Vec<u32> spoke-count, so the same shape now runs in
        // O(V) peak memory. The test asserts correctness against the
        // expected spoke partition; the memory contract is enforced by
        // the algorithm shape, not by an assertion.
        let mut tree = Tree { dirs: BTreeMap::new() };
        let dir = PathBuf::from("/pkg/src");
        let mut entry = crate::model::DirEntry::default();
        entry.modules.push(me("big", "big.ea"));

        const SPOKES: usize = 50;
        const CHAIN: usize = 4;
        let mut hub_callees: Vec<String> = Vec::with_capacity(SPOKES);
        for s in 0..SPOKES {
            hub_callees.push(format!("big.s{}_0", s));
        }
        let callee_refs: Vec<&str> = hub_callees.iter().map(String::as_str).collect();
        entry.functions.push(fe("big.hub", "big.ea", &callee_refs));

        for s in 0..SPOKES {
            for step in 0..CHAIN {
                let qname = format!("big.s{}_{}", s, step);
                let next_owned = if step + 1 < CHAIN {
                    Some(format!("big.s{}_{}", s, step + 1))
                } else {
                    None
                };
                let calls: Vec<&str> = match &next_owned {
                    Some(n) => vec![n.as_str()],
                    None => Vec::new(),
                };
                entry.functions.push(fe(&qname, "big.ea", &calls));
            }
        }

        tree.dirs.insert(dir.clone(), entry);
        let read = |p: &Path| if p == dir.join("big.ea") { Some(900) } else { None };
        let reports = analyze_file_cohesion(&tree, &read);
        assert_eq!(reports.len(), 1, "large hub-and-spoke should fire once");
        let r = &reports[0];
        assert_eq!(r.dispatcher.as_deref(), Some("big.hub"));
        assert_eq!(r.components.len(), SPOKES);
        for c in &r.components {
            assert_eq!(c.len(), CHAIN, "each spoke chain has {} functions", CHAIN);
        }
    }

    #[test]
    fn cohesion_suppressed_by_total_coverage_gate() {
        // Models parse/item/fn.ea: parse_fn_decl dispatches to 3 spoke
        // pairs (generics, params, body) of size 2 each. Total cluster
        // coverage = 6, below MIN_TOTAL_CLUSTER_COVERAGE (10). The lint
        // should NOT fire — splitting yields tiny sibling files that
        // are not worth the structural churn.
        let mut tree = Tree { dirs: BTreeMap::new() };
        let dir = PathBuf::from("/pkg/src");
        let mut entry = crate::model::DirEntry::default();
        entry.modules.push(me("fn", "fn.ea"));
        entry.functions.push(fe(
            "fn.parse_fn_decl",
            "fn.ea",
            &[
                "fn.parse_generics",
                "fn.parse_params",
                "fn.parse_body",
            ],
        ));
        entry.functions.push(fe("fn.parse_generics", "fn.ea", &["fn.parse_generic_entry"]));
        entry.functions.push(fe("fn.parse_generic_entry", "fn.ea", &[]));
        entry.functions.push(fe("fn.parse_params", "fn.ea", &["fn.parse_param"]));
        entry.functions.push(fe("fn.parse_param", "fn.ea", &[]));
        entry.functions.push(fe("fn.parse_body", "fn.ea", &["fn.parse_extern_body"]));
        entry.functions.push(fe("fn.parse_extern_body", "fn.ea", &[]));
        // Two non-clustered helpers to push n_funcs past MIN_FUNCTION_COUNT.
        entry.functions.push(fe("fn.merge_spans", "fn.ea", &[]));
        entry.functions.push(fe("fn.expect_name", "fn.ea", &[]));
        tree.dirs.insert(dir.clone(), entry);
        let read = |_p: &Path| Some(530);
        let reports = analyze_file_cohesion(&tree, &read);
        assert!(
            reports.is_empty(),
            "3 spoke pairs of size 2 (total = 6) should not fire — too small to justify a split"
        );
    }
