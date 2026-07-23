//! Emission-time `Severity`, manifest-input `LintSeverity`, and the
//! per-class `LintConfig` override table that maps between them.

use crate::class::{CLASS_COUNT, DiagnosticClass};

/// Emission-time severity of a built `Diagnostic`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Severity {
    /// Informational; build succeeds. The default for `gc_recoverable`.
    Info,
    /// Warning; build succeeds.
    Warn,
    /// Error; build fails (driver exit code != 0).
    Error,
}

impl Severity {
    /// `true` if this severity causes the build to fail.
    #[inline]
    pub const fn is_error(self) -> bool {
        matches!(self, Severity::Error)
    }

    /// Render as the lowercase word used in CLI output.
    pub const fn name(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// User-facing severity from `package.toml`'s `lints` block. Maps to an
/// emission-time [`Severity`] via [`Self::to_severity`]. There is no
/// `allow` variant — the opt-out feature was removed when every locked
/// diagnostic class became un-suppressible (the four structural lints
/// were the trigger; the broader removal is the same policy
/// reasoning extended to every class).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum LintSeverity {
    /// Emit as a warning.
    Warn,
    /// Emit as an error. `deny` is parsed into this variant per §3.
    Error,
}

impl LintSeverity {
    /// Project to the emission-time severity. Both variants map to the
    /// matching [`Severity`].
    pub const fn to_severity(self) -> Severity {
        match self {
            LintSeverity::Warn => Severity::Warn,
            LintSeverity::Error => Severity::Error,
        }
    }

    /// Parse a manifest-level severity name. Returns `None` for any name
    /// outside the locked §3 grammar.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "warn" => Some(LintSeverity::Warn),
            "deny" | "error" => Some(LintSeverity::Error),
            _ => None,
        }
    }
}

/// Per-class severity overrides composed from `package.toml`'s `lints` block
/// plus `--warn-as-error` CLI overrides (CLI applied last). Held by the
/// driver for the lifetime of a build invocation.
#[derive(Clone, Debug)]
pub struct LintConfig {
    overrides: [Option<LintSeverity>; CLASS_COUNT],
}

impl Default for LintConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl LintConfig {
    /// Construct a config with no overrides; every class resolves to its
    /// locked default severity.
    pub const fn new() -> Self {
        LintConfig {
            overrides: [None; CLASS_COUNT],
        }
    }

    /// Set the severity override for `class`. Later calls overwrite earlier
    /// ones — the driver applies manifest overrides first, then CLI ones.
    pub fn set(&mut self, class: DiagnosticClass, sev: LintSeverity) {
        self.overrides[class.index()] = Some(sev);
    }

    /// Read the override (if any) for `class`. `None` means "no override —
    /// the class will resolve to its locked default".
    pub fn get(&self, class: DiagnosticClass) -> Option<LintSeverity> {
        self.overrides[class.index()]
    }

    /// Compute the effective emission-time severity for `class`. Emission
    /// sites call this exactly once before building a `Diagnostic`.
    pub fn effective(&self, class: DiagnosticClass) -> Severity {
        match self.overrides[class.index()] {
            Some(sev) => sev.to_severity(),
            None => class.default_severity(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_names_and_is_error() {
        assert_eq!(Severity::Info.name(), "info");
        assert_eq!(Severity::Warn.name(), "warn");
        assert_eq!(Severity::Error.name(), "error");
        assert!(Severity::Error.is_error());
        assert!(!Severity::Warn.is_error());
        assert!(!Severity::Info.is_error());
        assert_eq!(format!("{}", Severity::Error), "error");
    }

    #[test]
    fn lint_severity_from_name_accepts_locked_grammar() {
        assert_eq!(LintSeverity::from_name("warn"), Some(LintSeverity::Warn));
        // `deny` and `error` collapse to the same variant per §3.
        assert_eq!(LintSeverity::from_name("deny"), Some(LintSeverity::Error));
        assert_eq!(LintSeverity::from_name("error"), Some(LintSeverity::Error));
        // `allow` is no longer in the grammar.
        assert_eq!(LintSeverity::from_name("allow"), None);
        // Anything outside the locked spellings is rejected.
        assert_eq!(LintSeverity::from_name(""), None);
        assert_eq!(LintSeverity::from_name("info"), None);
        assert_eq!(LintSeverity::from_name("forbid"), None);
        assert_eq!(LintSeverity::from_name("ERROR"), None);
    }

    #[test]
    fn lint_severity_to_severity() {
        assert_eq!(LintSeverity::Warn.to_severity(), Severity::Warn);
        assert_eq!(LintSeverity::Error.to_severity(), Severity::Error);
    }

    #[test]
    fn lint_config_default_returns_class_default() {
        let cfg = LintConfig::new();
        for class in DiagnosticClass::ALL {
            assert_eq!(cfg.effective(class), class.default_severity());
            assert_eq!(cfg.get(class), None);
        }
    }

    #[test]
    fn lint_config_override_replaces_default() {
        let mut cfg = LintConfig::new();
        // Escalate a warn-default class to error.
        cfg.set(DiagnosticClass::DeprecatedUse, LintSeverity::Error);
        assert_eq!(
            cfg.effective(DiagnosticClass::DeprecatedUse),
            Severity::Error
        );
        // Demote an error-default class to warn.
        cfg.set(DiagnosticClass::TypecheckError, LintSeverity::Warn);
        assert_eq!(
            cfg.effective(DiagnosticClass::TypecheckError),
            Severity::Warn
        );
        // Unrelated classes are still at their defaults.
        assert_eq!(
            cfg.effective(DiagnosticClass::ImportCycle),
            Severity::Error
        );
        assert_eq!(
            cfg.effective(DiagnosticClass::GcRecoverable),
            Severity::Info
        );
    }

    #[test]
    fn lint_config_later_set_overwrites_earlier() {
        let mut cfg = LintConfig::new();
        cfg.set(DiagnosticClass::UnusedImport, LintSeverity::Error);
        assert_eq!(
            cfg.get(DiagnosticClass::UnusedImport),
            Some(LintSeverity::Error)
        );
        // Later CLI/escalate call wins.
        cfg.set(DiagnosticClass::UnusedImport, LintSeverity::Warn);
        assert_eq!(
            cfg.get(DiagnosticClass::UnusedImport),
            Some(LintSeverity::Warn)
        );
        assert_eq!(cfg.effective(DiagnosticClass::UnusedImport), Severity::Warn);
    }
}
