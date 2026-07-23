//! Concrete generated value model for the property-test analyser.
//!
//! A [`Value`] is the primitive a generator strategy emits; the C9
//! runner serialises it into a synthesised Edda call site (literally as
//! a token in the generated test module's source).

// project at C9 serialisation time, not here
/// One concrete value the runner will substitute into a synthesised
/// call site. Admits the two integer-and-bool primitive shapes
/// the analyser produces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// Signed integer literal. Caller responsible for projecting to
    /// the param's actual width during serialisation.
    Int(i128),
    /// Boolean literal — `true` or `false`.
    Bool(bool),
}

impl Value {
    /// Render this value as the Edda source token a synthesised
    /// caller embeds at the call site (`123`, `-7`, `true`).
    pub fn render_source(&self) -> String {
        match self {
            Value::Int(v) => v.to_string(),
            Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_source_emits_integer_token() {
        assert_eq!(Value::Int(42).render_source(), "42");
        assert_eq!(Value::Int(-7).render_source(), "-7");
        assert_eq!(Value::Bool(true).render_source(), "true");
        assert_eq!(Value::Bool(false).render_source(), "false");
    }
}
