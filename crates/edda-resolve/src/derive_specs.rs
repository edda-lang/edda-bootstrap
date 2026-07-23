//! Mapping from `derive` item names to the stdlib spec each one
//! desugars into. Shared between the resolve-time auto-import injector
//! (`graph::collect_imports` enqueues the spec module so the source
//! graph reaches it) and the codegen-time desugar
//! (`edda-driver::codegen::collect_roots` builds one
//! [`edda_codegen::ArgumentTuple`] per `derive` item × target pair).
//!
//! Per `corpus/edda-codex/language/04-specs-comptime.md §5` the
//! whitelist is closed:
//!
//! | derive item   | spec module             | spec name      |
//! |---------------|-------------------------|----------------|
//! | `eq`          | `std.core.compare`      | `eq`           |
//! | `ord`         | `std.core.compare`      | `ord`          |
//! | `hash`        | `std.core.hash`         | `hash`         |
//! | `debug`       | `std.core.fmt`          | `debug`        |
//! | `clone`       | `std.core.copy`         | `clone`        |
//! | `properties`  | `std.testing.properties`| `properties`   |
//! | `serialize`   | `std.serde.core`        | `serialize`    |
//! | `deserialize` | `std.serde.core`        | `deserialize`  |
//!
//! The `module_segments` field is the segment list `std.<...>.<leaf>`
//! that names the stdlib module hosting the spec; the `spec_leaf` is
//! the spec declaration name within that module. The fully qualified
//! spec path is `<module_segments joined by '.'>.<spec_leaf>`.

/// One entry in the closed derive → stdlib spec table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DeriveSpecTarget {
    /// Dot-segmented module path (e.g. `["std", "core", "compare"]`).
    pub module_segments: &'static [&'static str],
    /// Spec declaration name within `module_segments` (e.g. `"eq"`).
    pub spec_leaf: &'static str,
}

impl DeriveSpecTarget {
    /// Fully-qualified dotted name `<module>.<spec_leaf>` — the input
    /// shape `find_spec_decl` matches against.
    pub fn qualified(&self) -> String {
        let mut out = String::with_capacity(64);
        for (i, seg) in self.module_segments.iter().enumerate() {
            if i > 0 {
                out.push('.');
            }
            out.push_str(seg);
        }
        out.push('.');
        out.push_str(self.spec_leaf);
        out
    }
}

/// Look up the stdlib spec target for a `derive` item name.
pub fn derive_spec_target(name: &str) -> Option<DeriveSpecTarget> {
    Some(match name {
        "eq" => DeriveSpecTarget {
            module_segments: &["std", "core", "compare"],
            spec_leaf: "eq",
        },
        "ord" => DeriveSpecTarget {
            module_segments: &["std", "core", "compare"],
            spec_leaf: "ord",
        },
        "hash" => DeriveSpecTarget {
            module_segments: &["std", "core", "hash"],
            spec_leaf: "hash",
        },
        "debug" => DeriveSpecTarget {
            module_segments: &["std", "core", "fmt"],
            spec_leaf: "debug",
        },
        "clone" => DeriveSpecTarget {
            module_segments: &["std", "core", "copy"],
            spec_leaf: "clone",
        },
        "properties" => DeriveSpecTarget {
            module_segments: &["std", "testing", "properties"],
            spec_leaf: "properties",
        },
        "serialize" => DeriveSpecTarget {
            module_segments: &["std", "serde", "core"],
            spec_leaf: "serialize",
        },
        "deserialize" => DeriveSpecTarget {
            module_segments: &["std", "serde", "core"],
            spec_leaf: "deserialize",
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_maps_to_compare_module() {
        let t = derive_spec_target("eq").unwrap();
        assert_eq!(t.module_segments, &["std", "core", "compare"]);
        assert_eq!(t.spec_leaf, "eq");
        assert_eq!(t.qualified(), "std.core.compare.eq");
    }

    #[test]
    fn hash_maps_to_hash_module() {
        let t = derive_spec_target("hash").unwrap();
        assert_eq!(t.qualified(), "std.core.hash.hash");
    }

    #[test]
    fn unknown_returns_none() {
        assert!(derive_spec_target("typo").is_none());
        assert!(derive_spec_target("Eq").is_none());
    }

    #[test]
    fn every_whitelist_item_resolves() {
        for name in [
            "eq",
            "ord",
            "hash",
            "debug",
            "clone",
            "properties",
            "serialize",
            "deserialize",
        ] {
            assert!(derive_spec_target(name).is_some(), "{name}");
        }
    }
}
