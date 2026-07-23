//! Unit tests for the `package.toml` parser (§3).

    use super::*;
    use crate::schema::{
        Compression, DebugInfo, DefaultTier, DependencySource, GcTag, GlobalCache,
        WorkspaceDiscover,
    };
    use edda_diag::{LintSeverity, Severity};
    use edda_span::SourceMap;
    use std::path::PathBuf;

    fn parse_for_test(src: &str) -> (Option<PackageManifest>, Diagnostics) {
        let map = SourceMap::new();
        let file = map.add_file(PathBuf::from("package.toml"), src.to_owned());
        let mut diags = Diagnostics::new();
        let cfg = LintConfig::new();
        let m = parse(src, file, &mut diags, &cfg);
        (m, diags)
    }

    const MINIMAL: &str = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"
"#;

    #[test]
    fn minimal_manifest_parses() {
        let (m, diags) = parse_for_test(MINIMAL);
        assert!(diags.is_empty(), "got: {:?}", diags.iter().collect::<Vec<_>>());
        let m = m.unwrap();
        assert_eq!(m.package.as_ref(), "my_project");
        assert_eq!(m.version.major, 0);
        assert_eq!(m.version.minor, 1);
        assert_eq!(m.version.patch, 0);
        assert_eq!(m.root_namespace.as_ref(), "my_project");
        assert!(m.dependencies.is_empty());
        assert_eq!(m.build.default_profile.as_ref(), "dev");
        assert!(m.build.default_target.is_none());
        assert!(m.build.default_features.is_empty());
        assert_eq!(m.profiles.len(), 3);
    }

    #[test]
    fn missing_required_field_fails() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.has_errors());
    }

    #[test]
    fn bad_semver_rejected() {
        let src = r#"
[package]
name = "my_project"
version = "1.2"
root_namespace = "my_project"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.has_errors());
    }

    #[test]
    fn package_name_allows_hyphens() {
        let src = r#"
[package]
name = "my-project"
version = "0.1.0"
root_namespace = "my_project"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(!diags.has_errors());
        assert_eq!(m.unwrap().package.as_ref(), "my-project");
    }

    #[test]
    fn root_namespace_must_be_snake_case() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my-project"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.has_errors());
    }

    #[test]
    fn kind_absent_defaults_to_none() {
        let (m, diags) = parse_for_test(MINIMAL);
        assert!(!diags.has_errors());
        assert!(m.unwrap().kind.is_none());
    }

    #[test]
    fn kind_accepts_all_three_locked_values() {
        for (text, expect) in [
            ("executable", crate::schema::PackageKind::Executable),
            ("static_library", crate::schema::PackageKind::StaticLibrary),
            ("dynamic_library", crate::schema::PackageKind::DynamicLibrary),
        ] {
            let src = format!(
                r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"
kind = "{text}"
"#
            );
            let (m, diags) = parse_for_test(&src);
            assert!(!diags.has_errors(), "kind {text:?} got: {:?}", diags.iter().collect::<Vec<_>>());
            assert_eq!(m.unwrap().kind, Some(expect));
        }
    }

    #[test]
    fn kind_rejects_unknown_value() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"
kind = "shared_object"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.has_errors());
    }

    #[test]
    fn dependencies_parsed() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[[dependencies]]
name = "json"
version = "^1.0.0"
source = "registry"

[[dependencies]]
name = "graphics"
version = ">=2.3.0"
source = "git+https://example.com/g"

[[dependencies]]
name = "local"
version = "0.1.0"
source = "path+../local"
"#;
        let (m, _) = parse_for_test(src);
        let m = m.unwrap();
        assert_eq!(m.dependencies.len(), 3);
        assert_eq!(m.dependencies[0].name.as_ref(), "json");
        assert_eq!(m.dependencies[0].version_req.as_ref(), "^1.0.0");
        assert_eq!(m.dependencies[0].source, DependencySource::Registry);
        assert!(matches!(
            &m.dependencies[1].source,
            DependencySource::Git(u) if u.as_ref() == "https://example.com/g"
        ));
        assert!(matches!(
            &m.dependencies[2].source,
            DependencySource::Path(p) if p.as_ref() == "../local"
        ));
    }

    #[test]
    fn build_block_parsed() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[build]
default_target = "x86-64-linux-gnu"
default_features = ["avx2", "sse4.2"]
default_profile = "release"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(!diags.has_errors());
        let m = m.unwrap();
        assert_eq!(m.build.default_target.unwrap().to_string(), "x86-64-linux-gnu");
        let feats: Vec<_> = m.build.default_features.iter().map(|f| f.name.as_ref()).collect();
        assert_eq!(feats, vec!["avx2", "sse4.2"]);
        assert_eq!(m.build.default_profile.as_ref(), "release");
    }

    #[test]
    fn unknown_feature_for_arch_emits_warning() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[build]
default_target = "aarch64-linux-gnu"
default_features = ["avx2"]
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_some());
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::UnknownTargetFeature));
    }

    #[test]
    fn profiles_replace_locked_defaults() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[profiles.dev]
opt_level = 2
debug_info = "full"
sanitizers = ["address"]

[profiles.fuzz]
opt_level = 1
debug_info = "full"
sanitizers = ["address"]
"#;
        let (m, diags) = parse_for_test(src);
        assert!(!diags.has_errors(), "got: {:?}", diags.iter().collect::<Vec<_>>());
        let m = m.unwrap();
        let dev = m.profiles.get("dev").unwrap();
        assert_eq!(dev.opt_level, 2);
        let release = m.profiles.get("release").unwrap();
        assert_eq!(release.opt_level, 3);
        let fuzz = m.profiles.get("fuzz").unwrap();
        assert_eq!(fuzz.opt_level, 1);
    }

    #[test]
    fn lints_block_populates_lint_config() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[lints]
stable_contract_revision = "error"
unused_import = "warn"
deprecated_use = "deny"
"#;
        let (m, _) = parse_for_test(src);
        let m = m.unwrap();
        assert_eq!(m.lints.get(DiagnosticClass::StableContractRevision), Some(LintSeverity::Error));
        assert_eq!(m.lints.get(DiagnosticClass::UnusedImport), Some(LintSeverity::Warn));
        assert_eq!(m.lints.get(DiagnosticClass::DeprecatedUse), Some(LintSeverity::Error));
    }

    #[test]
    fn lints_allow_severity_rejected_with_parse_error() {
        // The `allow`
        // opt-out feature was removed; setting any class to `allow`
        // now surfaces as a `parse_error` and the override is dropped.
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[lints]
unused_import = "allow"
"#;
        let (m, diags) = parse_for_test(src);
        let m = m.unwrap();
        assert!(diags.iter().any(|d| {
            d.class == DiagnosticClass::ParseError && d.message.contains("`allow`")
        }));
        assert_eq!(m.lints.get(DiagnosticClass::UnusedImport), None);
    }

    #[test]
    fn codegen_block_parsed() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[codegen]
default_tier = "cache"
global_cache = "enabled"
compression = "zstd"

[codegen.gc_schedule]
cache_tier = "daily"
repo_tier = "never"
global_cache = "weekly"
"#;
        let (m, _) = parse_for_test(src);
        let m = m.unwrap();
        assert_eq!(m.codegen.default_tier, DefaultTier::Cache);
        assert_eq!(m.codegen.global_cache, GlobalCache::Enabled);
        assert_eq!(m.codegen.compression, Compression::Zstd);
        assert_eq!(m.codegen.gc_schedule.cache_tier, GcTag::Daily);
        assert_eq!(m.codegen.gc_schedule.global_cache, GcTag::Weekly);
    }

    #[test]
    fn reserved_manifest_key_emits_unknown_manifest_key() {
        // unknown_manifest_key defaults
        // to Error rather than Warn. A reserved future-feature section
        // still surfaces through this class because it's not (yet) in
        // the locked schema; the diagnostic now blocks the build.
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[build_script]
path = "scripts/build.ea"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_some());
        let errors: Vec<_> = diags.iter()
            .filter(|d| d.class == DiagnosticClass::UnknownManifestKey)
            .collect();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, Severity::Error);
    }

    #[test]
    fn workspace_members_parsed() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
members = ["foo", "bar"]
"#;
        let (m, diags) = parse_for_test(src);
        let m = m.unwrap();
        assert!(!diags.iter().any(|d| d.class == DiagnosticClass::UnknownManifestKey),
            "unexpected unknown-key warning(s): {:?}", diags.iter().collect::<Vec<_>>());
        let ws = m.workspace.expect("workspace table populated");
        let names: Vec<&str> = ws.members.iter().map(|s| s.as_ref()).collect();
        assert_eq!(names, vec!["foo", "bar"]);
    }

    #[test]
    fn workspace_empty_members_rejected() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
members = []
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
    }

    #[test]
    fn workspace_member_nested_path_accepted() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
members = ["core/option", "crypto/aead/aes_gcm"]
"#;
        let (m, diags) = parse_for_test(src);
        let m = m.expect("nested member paths are admitted (B-010)");
        assert!(!diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
        let ws = m.workspace.as_ref().expect("[workspace] populated");
        let names: Vec<&str> = ws.members.iter().map(|s| s.as_ref()).collect();
        assert_eq!(names, vec!["core/option", "crypto/aead/aes_gcm"]);
    }

    #[test]
    fn workspace_member_backslash_rejected() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
members = ["foo\\bar"]
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
    }

    #[test]
    fn workspace_member_path_traversal_rejected() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
members = ["foo/../bar"]
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
    }

    #[test]
    fn workspace_member_absolute_path_rejected() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
members = ["/foo"]
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
    }

    #[test]
    fn workspace_discover_true_accepted() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
discover = true
"#;
        let (m, diags) = parse_for_test(src);
        let m = m.expect("[workspace] discover = true is admitted (B-011)");
        assert!(!diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
        let ws = m.workspace.as_ref().expect("[workspace] populated");
        assert!(ws.members.is_empty());
        assert_eq!(ws.discover, Some(WorkspaceDiscover::LibRoot));
    }

    #[test]
    fn workspace_discover_path_accepted() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
discover = "packages/runtime"
"#;
        let (m, diags) = parse_for_test(src);
        let m = m.expect("[workspace] discover = \"<path>\" is admitted (B-011)");
        assert!(!diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
        let ws = m.workspace.as_ref().expect("[workspace] populated");
        assert_eq!(
            ws.discover,
            Some(WorkspaceDiscover::Path("packages/runtime".into()))
        );
    }

    #[test]
    fn workspace_discover_and_members_mutually_exclusive() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
discover = true
members = ["foo"]
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError
            && d.message.contains("members") && d.message.contains("discover")));
    }

    #[test]
    fn workspace_discover_false_rejected() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
discover = false
members = ["foo"]
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
    }

    #[test]
    fn workspace_discover_forces_descendant_tree() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
discover = true
"#;
        let (m, _) = parse_for_test(src);
        let m = m.expect("discover-only manifest parses");
        // Explicit [structmap] omitted — locked defaults set descendant_tree
        // to false, but B-011's implication overrides it to true.
        assert!(m.structmap.descendant_tree);
    }

    #[test]
    fn workspace_requires_members_or_discover() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError
            && d.message.contains("members") && d.message.contains("discover")));
    }

    #[test]
    fn workspace_duplicate_member_rejected() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
members = ["foo", "foo"]
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
    }

    #[test]
    fn workspace_default_run_parsed() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
members = ["foo", "bar"]
default_run = "foo"
"#;
        let (m, diags) = parse_for_test(src);
        let m = m.expect("[workspace] default_run is admitted");
        assert!(!diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
        assert!(!diags.iter().any(|d| d.class == DiagnosticClass::UnknownManifestKey));
        let ws = m.workspace.as_ref().expect("[workspace] populated");
        assert_eq!(ws.default_run.as_deref(), Some("foo"));
    }

    #[test]
    fn workspace_default_run_absent_is_none() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
members = ["foo", "bar"]
"#;
        let (m, _) = parse_for_test(src);
        let m = m.unwrap();
        let ws = m.workspace.as_ref().expect("[workspace] populated");
        assert_eq!(ws.default_run, None);
    }

    #[test]
    fn workspace_default_run_nested_path_accepted() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
members = ["core/option", "crypto/aead/aes_gcm"]
default_run = "core/option"
"#;
        let (m, diags) = parse_for_test(src);
        let m = m.expect("nested default_run path admitted in the members shape");
        assert!(!diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
        let ws = m.workspace.as_ref().expect("[workspace] populated");
        assert_eq!(ws.default_run.as_deref(), Some("core/option"));
    }

    #[test]
    fn workspace_default_run_backslash_rejected() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
members = ["foo"]
default_run = "foo\\bar"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
    }

    #[test]
    fn workspace_default_run_non_string_rejected() {
        let src = r#"
[package]
name = "my_workspace_root"
version = "0.1.0"
root_namespace = "my_workspace_root"

[workspace]
members = ["foo"]
default_run = true
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError));
    }

    #[test]
    fn unknown_lint_class_warns_and_continues() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[lints]
not_a_real_class = "warn"
unused_import = "error"
"#;
        let (m, diags) = parse_for_test(src);
        let m = m.unwrap();
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::UnknownManifestKey));
        assert_eq!(m.lints.get(DiagnosticClass::UnusedImport), Some(LintSeverity::Error));
    }

    #[test]
    fn semver_with_pre_release_and_build() {
        let src = r#"
[package]
name = "p"
version = "1.2.3-rc.1+sha.abc"
root_namespace = "p"
"#;
        let (m, _) = parse_for_test(src);
        let v = m.unwrap().version;
        assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));
        assert_eq!(v.pre_release.as_deref(), Some("rc.1"));
        assert_eq!(v.build.as_deref(), Some("sha.abc"));
    }

    #[test]
    fn invalid_target_in_build_rejected() {
        let src = r#"
[package]
name = "p"
version = "0.1.0"
root_namespace = "p"

[build]
default_target = "not-a-triple"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.has_errors());
    }

    #[test]
    fn reserved_root_namespace_rejected() {
        for reserved in &["std", "codegen", "tests", "bench", "examples"] {
            let src = format!(r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "{reserved}"
"#);
            let (m, diags) = parse_for_test(&src);
            assert!(m.is_none(), "{reserved} should be rejected");
            assert!(diags.has_errors(), "{reserved} should emit an error");
        }
    }

    #[test]
    fn package_can_be_reserved_namespace_name() {
        let src = r#"
[package]
name = "std"
version = "0.1.0"
root_namespace = "my_project"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(!diags.has_errors());
        assert_eq!(m.unwrap().package.as_ref(), "std");
    }

    #[test]
    fn duplicate_dependency_name_rejected() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[[dependencies]]
name = "json"
version = "1.0.0"
source = "registry"

[[dependencies]]
name = "json"
version = "2.0.0"
source = "registry"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.iter().any(|d| d.message.contains("duplicate dependency")));
    }

    #[test]
    fn load_reads_file_and_parses() {
        let dir = std::env::temp_dir().join(format!("edda-manifest-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("package.toml");
        std::fs::write(&path, MINIMAL).unwrap();
        let map = SourceMap::new();
        let mut diags = Diagnostics::new();
        let cfg = LintConfig::new();
        let m = load(&path, &map, &mut diags, &cfg).unwrap();
        assert_eq!(m.package.as_ref(), "my_project");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn load_missing_file_emits_parse_error() {
        let path = std::env::temp_dir()
            .join(format!("edda-manifest-missing-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let map = SourceMap::new();
        let mut diags = Diagnostics::new();
        let cfg = LintConfig::new();
        let m = load(&path, &map, &mut diags, &cfg);
        assert!(m.is_none());
        assert!(diags.has_errors());
        assert!(diags.iter().any(|d| {
            d.class == DiagnosticClass::ParseError && d.message.contains("cannot read manifest")
        }));
    }

    #[test]
    fn structmap_defaults_when_table_omitted() {
        let (m, _) = parse_for_test(MINIMAL);
        let m = m.unwrap();
        assert_eq!(m.structmap, StructmapConfig::locked_defaults());
        assert!(!m.structmap.descendant_tree);
    }

    #[test]
    fn structmap_descendant_tree_parsed() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[structmap]
descendant_tree = true
"#;
        let (m, diags) = parse_for_test(src);
        assert!(!diags.has_errors());
        assert!(m.unwrap().structmap.descendant_tree);
    }

    #[test]
    fn structmap_unknown_key_warns() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[structmap]
not_a_real_key = true
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_some());
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::UnknownManifestKey));
    }

    #[test]
    fn structmap_non_bool_descendant_tree_rejected() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[structmap]
descendant_tree = "yes"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.has_errors());
    }

    #[test]
    fn full_spec_example_parses() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[[dependencies]]
name = "json"
version = "1.0.2"
source = "registry"

[[dependencies]]
name = "graphics"
version = "2.3.0"
source = "git+https://github.com/example/graphics"

[build]
default_target = "x86-64-linux-gnu"
default_features = ["avx2", "sse4.2"]
default_profile = "dev"

[profiles.dev]
opt_level = 0
debug_info = "full"
sanitizers = ["address"]

[profiles.release]
opt_level = 3
debug_info = "line-tables-only"
sanitizers = []

[profiles.bench]
opt_level = 3
debug_info = "full"
sanitizers = []

[lints]
stable_contract_revision = "error"
unaligned_field_access = "warn"
comptime_purity_loss = "deny"

[codegen]
default_tier = "cache"
compression = "false"
global_cache = "enabled"

[codegen.gc_schedule]
cache_tier = "weekly"
repo_tier = "never"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(!diags.has_errors(), "spec example produced errors: {:?}", diags.iter().collect::<Vec<_>>());
        let m = m.unwrap();
        assert_eq!(m.package.as_ref(), "my_project");
        assert_eq!(m.dependencies.len(), 2);
        assert_eq!(m.dependencies[1].source, DependencySource::Git(
            "https://github.com/example/graphics".into()
        ));
        assert_eq!(m.build.default_target.unwrap().to_string(), "x86-64-linux-gnu");
        let dev = m.profiles.get("dev").unwrap();
        assert_eq!(dev.opt_level, 0);
        assert_eq!(dev.debug_info, DebugInfo::Full);
        assert_eq!(dev.sanitizers.iter().map(|s| s.as_ref()).collect::<Vec<_>>(), vec!["address"]);
        let release = m.profiles.get("release").unwrap();
        assert!(release.sanitizers.is_empty());
        assert_eq!(m.lints.get(DiagnosticClass::StableContractRevision), Some(LintSeverity::Error));
        assert_eq!(m.codegen.default_tier, DefaultTier::Cache);
        assert_eq!(m.codegen.global_cache, GlobalCache::Enabled);
        assert_eq!(m.codegen.compression, Compression::None);
        assert_eq!(m.codegen.gc_schedule.cache_tier, GcTag::Weekly);
        assert_eq!(m.codegen.gc_schedule.repo_tier, GcTag::Never);
        assert_eq!(m.codegen.gc_schedule.global_cache, GcTag::Never);
    }

    // ---- workspace-only (`[workspace]` without `[package]`) coverage ----

    #[test]
    fn parse_any_workspace_only_accepted() {
        let src = r#"
[workspace]
members = ["foo", "bar"]
"#;
        let map = SourceMap::new();
        let file = map.add_file(PathBuf::from("package.toml"), src.to_owned());
        let mut diags = Diagnostics::new();
        let cfg = LintConfig::new();
        let loaded = parse_any(src, file, &mut diags, &cfg).expect("workspace-only must parse");
        assert!(!diags.has_errors(), "got: {:?}", diags.iter().collect::<Vec<_>>());
        match loaded {
            LoadedManifest::WorkspaceOnly(w) => {
                let names: Vec<&str> = w.workspace.members.iter().map(|s| s.as_ref()).collect();
                assert_eq!(names, vec!["foo", "bar"]);
            }
            LoadedManifest::Package(_) => panic!("expected WorkspaceOnly, got Package"),
        }
    }

    #[test]
    fn parse_rejects_workspace_only() {
        let src = r#"
[workspace]
members = ["foo"]
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none(), "legacy `parse` must reject workspace-only");
        assert!(diags.has_errors());
        assert!(diags.iter().any(|d| d.message.contains("workspace-only")));
    }

    #[test]
    fn parse_any_package_only_round_trips() {
        let map = SourceMap::new();
        let file = map.add_file(PathBuf::from("package.toml"), MINIMAL.to_owned());
        let mut diags = Diagnostics::new();
        let cfg = LintConfig::new();
        let loaded = parse_any(MINIMAL, file, &mut diags, &cfg).unwrap();
        assert!(!diags.has_errors());
        assert!(matches!(loaded, LoadedManifest::Package(_)));
        match loaded {
            LoadedManifest::Package(p) => assert_eq!(p.package.as_ref(), "my_project"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn parse_any_still_requires_package_when_no_workspace() {
        let src = r#"
[build]
default_profile = "dev"
"#;
        let map = SourceMap::new();
        let file = map.add_file(PathBuf::from("package.toml"), src.to_owned());
        let mut diags = Diagnostics::new();
        let cfg = LintConfig::new();
        let loaded = parse_any(src, file, &mut diags, &cfg);
        assert!(loaded.is_none(), "neither [package] nor [workspace] is an error");
        assert!(diags.has_errors());
    }

    #[test]
    fn load_any_workspace_only_with_no_src_accepted() {
        let dir = std::env::temp_dir()
            .join(format!("edda-manifest-ws-only-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("package.toml");
        std::fs::write(&path, "[workspace]\nmembers = [\"foo\"]\n").unwrap();
        let map = SourceMap::new();
        let mut diags = Diagnostics::new();
        let cfg = LintConfig::new();
        let loaded = load_any(&path, &map, &mut diags, &cfg).expect("workspace-only must load");
        assert!(loaded.is_workspace_only(), "expected WorkspaceOnly");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_any_workspace_only_with_src_rejected() {
        let dir = std::env::temp_dir()
            .join(format!("edda-manifest-hybrid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let path = dir.join("package.toml");
        std::fs::write(&path, "[workspace]\nmembers = [\"foo\"]\n").unwrap();
        let map = SourceMap::new();
        let mut diags = Diagnostics::new();
        let cfg = LintConfig::new();
        let loaded = load_any(&path, &map, &mut diags, &cfg);
        assert!(loaded.is_none(), "src/ alongside workspace-only must reject");
        assert!(diags.has_errors());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- Mímir dep-field tests (§6.2–§6.5) ----

    #[test]
    fn dependency_with_full_mimir_block_round_trips() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[[dependencies]]
name = "crypto"
version = "^1.2.0"
source = "registry"
surface_hash = "blake3:deadbeefcafe0011"
max_effects = ["err: alloc.AllocError", "cancellation"]
accept_unstable = true
publisher = { key_fingerprint = "ed25519:abcdef0123456789" }
"#;
        let (m, diags) = parse_for_test(src);
        assert!(!diags.has_errors(), "got: {:?}", diags.iter().collect::<Vec<_>>());
        let m = m.unwrap();
        assert_eq!(m.dependencies.len(), 1);
        let dep = &m.dependencies[0];
        assert_eq!(dep.name.as_ref(), "crypto");
        assert_eq!(dep.surface_hash.as_deref(), Some("blake3:deadbeefcafe0011"));
        assert_eq!(
            dep.max_effects.iter().map(|s| s.as_ref()).collect::<Vec<_>>(),
            vec!["err: alloc.AllocError", "cancellation"]
        );
        assert!(dep.accept_unstable);
        let pin = dep.publisher.as_ref().expect("publisher pin present");
        assert_eq!(pin.key_fingerprint.as_ref(), "ed25519:abcdef0123456789");
    }

    #[test]
    fn dependency_with_empty_max_effects_admitted() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[[dependencies]]
name = "pure_lib"
version = "1.0.0"
source = "registry"
max_effects = []
"#;
        let (m, diags) = parse_for_test(src);
        assert!(!diags.has_errors(), "empty max_effects must parse cleanly");
        let m = m.unwrap();
        assert_eq!(m.dependencies[0].max_effects.len(), 0);
    }

    #[test]
    fn dependency_omitting_surface_hash_yields_none() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[[dependencies]]
name = "foo"
version = "0.1.0"
source = "registry"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(!diags.has_errors());
        let m = m.unwrap();
        assert!(m.dependencies[0].surface_hash.is_none(),
            "omitted surface_hash must be None (first-install state)");
    }

    #[test]
    fn dependency_with_malformed_surface_hash_emits_parse_error() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[[dependencies]]
name = "foo"
version = "0.1.0"
source = "registry"
surface_hash = "not-a-prefix:xxx"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none(), "malformed surface_hash must fail parse");
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError),
            "expected a parse_error diagnostic");
    }

    #[test]
    fn dependency_with_malformed_publisher_fingerprint_emits_parse_error() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[[dependencies]]
name = "foo"
version = "0.1.0"
source = "registry"
publisher = { key_fingerprint = "bad" }
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none(), "malformed key_fingerprint must fail parse");
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError),
            "expected a parse_error diagnostic");
    }

    #[test]
    fn dependency_with_empty_publisher_block_emits_parse_error() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[[dependencies]]
name = "foo"
version = "0.1.0"
source = "registry"
publisher = {}
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none(), "empty [publisher] block must fail parse");
        assert!(diags.iter().any(|d| d.class == DiagnosticClass::ParseError),
            "expected a parse_error diagnostic for missing key_fingerprint");
    }

    #[test]
    fn dependency_default_accept_unstable_is_false() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"

[[dependencies]]
name = "foo"
version = "0.1.0"
source = "registry"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(!diags.has_errors());
        let m = m.unwrap();
        assert!(!m.dependencies[0].accept_unstable,
            "omitted accept_unstable must default to false");
    }

    #[test]
    fn load_rejects_workspace_only_via_load() {
        let dir = std::env::temp_dir()
            .join(format!("edda-manifest-load-rejects-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("package.toml");
        std::fs::write(&path, "[workspace]\nmembers = [\"foo\"]\n").unwrap();
        let map = SourceMap::new();
        let mut diags = Diagnostics::new();
        let cfg = LintConfig::new();
        let loaded = load(&path, &map, &mut diags, &cfg);
        assert!(loaded.is_none(), "legacy load() must keep returning PackageManifest only");
        assert!(diags.iter().any(|d| d.message.contains("workspace-only")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_descriptive_keys_parsed_and_retained() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"
edition = "2026"
authors = ["Ada Lovelace", "Grace Hopper"]
license = "MIT OR Apache-2.0"
description = "A demonstration package."
"#;
        let (m, diags) = parse_for_test(src);
        assert!(!diags.has_errors(), "got: {:?}", diags.iter().collect::<Vec<_>>());
        let m = m.unwrap();
        assert_eq!(m.edition.as_deref(), Some("2026"));
        assert_eq!(
            m.authors.iter().map(|a| a.as_ref()).collect::<Vec<_>>(),
            vec!["Ada Lovelace", "Grace Hopper"]
        );
        assert_eq!(m.license.as_deref(), Some("MIT OR Apache-2.0"));
        assert_eq!(m.description.as_deref(), Some("A demonstration package."));
    }

    #[test]
    fn package_descriptive_keys_default_when_absent() {
        let (m, _diags) = parse_for_test(MINIMAL);
        let m = m.unwrap();
        assert!(m.edition.is_none());
        assert!(m.authors.is_empty());
        assert!(m.license.is_none());
        assert!(m.description.is_none());
    }

    #[test]
    fn package_authors_must_be_array_of_strings() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"
authors = "Ada Lovelace"
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.has_errors());
    }

    #[test]
    fn package_description_must_be_string() {
        let src = r#"
[package]
name = "my_project"
version = "0.1.0"
root_namespace = "my_project"
description = 42
"#;
        let (m, diags) = parse_for_test(src);
        assert!(m.is_none());
        assert!(diags.has_errors());
    }
