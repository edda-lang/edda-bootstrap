//! Type-schema registry consumed by the Z3 translator.
//!
//! Refinement-decidability.md §2 models record-field projection as an
//! uninterpreted function per record-field and admits structural equality on
//! tuples and records of decidable-type components. To get that structural
//! equality "for free" we lower records and sums to Z3 datatypes — which need
//! the field/variant list at declaration time.
//!
//! [`Schema`] is the bridge between the typechecker (which knows every
//! record's field list and every sum's variant set) and refine's translator
//! (which builds the Z3 datatypes lazily). The typechecker integration is
//! the `edda-types` HIR adapter.
//!
//! Empty schemas are valid — programs whose predicates touch no records and
//! no sums (the LIA + Bool + Array fragment) discharge against an
//! empty schema.

use std::collections::HashMap;

use smol_str::SmolStr;

use crate::sort::{RecordRef, Sort, SumRef};

//            positional so reordering after declaration is a breaking change
/// Field list for a record type. Order is the source-level declaration order;
/// Z3 datatype constructors are positional, so two schemas with the same
/// fields in different orders produce distinct datatypes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordSchema {
    /// Record name; matches the [`RecordRef`] passed to
    /// [`Predicate::FieldProj`](crate::Predicate::FieldProj).
    pub name: SmolStr,
    /// Field-name → field-sort pairs, in source order.
    pub fields: Vec<(SmolStr, Sort)>,
}

impl RecordSchema {
    /// Construct a schema with the given name and field list.
    pub fn new(name: impl Into<SmolStr>, fields: Vec<(SmolStr, Sort)>) -> RecordSchema {
        RecordSchema {
            name: name.into(),
            fields,
        }
    }

    /// Look up a field's sort by name. Returns `None` if the field is not in
    /// the schema.
    pub fn field_sort(&self, field: &str) -> Option<&Sort> {
        self.fields
            .iter()
            .find(|(name, _)| name.as_str() == field)
            .map(|(_, sort)| sort)
    }

    /// Find the positional index of a field. Used by the Z3 translator to
    /// pick the right accessor from the [`DatatypeSort`].
    pub fn field_index(&self, field: &str) -> Option<usize> {
        self.fields
            .iter()
            .position(|(name, _)| name.as_str() == field)
    }
}

//            positional argument list mirrors this
/// One variant of a sum type. Payload is empty for the tag-only case
/// (`closed` in `Connection { closed, open(socket_fd: i32) }`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariantSchema {
    /// Variant name.
    pub name: SmolStr,
    /// Payload field list in declaration order. Empty for payload-free.
    pub payload: Vec<(SmolStr, Sort)>,
}

impl VariantSchema {
    /// Payload-free variant.
    pub fn tag(name: impl Into<SmolStr>) -> VariantSchema {
        VariantSchema {
            name: name.into(),
            payload: Vec::new(),
        }
    }

    /// Variant with the given payload fields.
    pub fn with_payload(
        name: impl Into<SmolStr>,
        payload: Vec<(SmolStr, Sort)>,
    ) -> VariantSchema {
        VariantSchema {
            name: name.into(),
            payload,
        }
    }

    /// `true` if this variant carries no payload — admissible for tag-equality
    /// refinements per refinement-decidability.md §5.
    pub fn is_payload_free(&self) -> bool {
        self.payload.is_empty()
    }
}

//            positional and the order seeds the tester / accessor indexing
/// Variant list for a sum type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SumSchema {
    /// Sum-type name; matches the [`SumRef`] passed to
    /// [`Predicate::TagEq`](crate::Predicate::TagEq).
    pub name: SmolStr,
    /// Variants in source order.
    pub variants: Vec<VariantSchema>,
}

impl SumSchema {
    /// Construct a sum schema.
    pub fn new(name: impl Into<SmolStr>, variants: Vec<VariantSchema>) -> SumSchema {
        SumSchema {
            name: name.into(),
            variants,
        }
    }

    /// Find a variant by name.
    pub fn variant(&self, name: &str) -> Option<&VariantSchema> {
        self.variants.iter().find(|v| v.name.as_str() == name)
    }

    /// Find the positional index of a variant.
    pub fn variant_index(&self, name: &str) -> Option<usize> {
        self.variants
            .iter()
            .position(|v| v.name.as_str() == name)
    }
}

//            translated to a Z3 datatype, mutating its schema would produce a
//            stale datatype handle in the translator cache
/// Registry of every record and sum the refine layer needs to translate.
/// Build it with [`Schema::empty`] + [`Schema::with_record`] /
/// [`Schema::with_sum`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Schema {
    records: HashMap<SmolStr, RecordSchema>,
    sums: HashMap<SmolStr, SumSchema>,
}

impl Schema {
    /// Construct an empty schema. Discharges against an empty schema work
    /// for any predicate that touches no records and no sums.
    pub fn empty() -> Schema {
        Schema::default()
    }

    /// Builder-style: append a record schema.
    pub fn with_record(mut self, schema: RecordSchema) -> Schema {
        self.records.insert(schema.name.clone(), schema);
        self
    }

    /// Builder-style: append a sum schema.
    pub fn with_sum(mut self, schema: SumSchema) -> Schema {
        self.sums.insert(schema.name.clone(), schema);
        self
    }

    /// Look up a record schema by [`RecordRef`].
    pub fn record(&self, record: &RecordRef) -> Option<&RecordSchema> {
        self.records.get(record.name())
    }

    /// Look up a sum schema by [`SumRef`].
    pub fn sum(&self, sum: &SumRef) -> Option<&SumSchema> {
        self.sums.get(sum.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::IntSort;

    fn i32_sort() -> Sort {
        Sort::Int(IntSort::sized(crate::sort::IntWidth::W32, true))
    }

    #[test]
    fn record_schema_indexes_fields_by_name_and_position() {
        let r = RecordSchema::new(
            "Point",
            vec![("x".into(), i32_sort()), ("y".into(), i32_sort())],
        );
        assert_eq!(r.field_index("x"), Some(0));
        assert_eq!(r.field_index("y"), Some(1));
        assert_eq!(r.field_index("z"), None);
        assert!(r.field_sort("x").is_some());
    }

    #[test]
    fn variant_schema_distinguishes_tag_from_payload() {
        let tag = VariantSchema::tag("closed");
        assert!(tag.is_payload_free());
        let payload = VariantSchema::with_payload(
            "open",
            vec![("socket_fd".into(), i32_sort())],
        );
        assert!(!payload.is_payload_free());
    }

    #[test]
    fn schema_round_trips_record_and_sum_lookup() {
        let schema = Schema::empty()
            .with_record(RecordSchema::new(
                "Point",
                vec![("x".into(), i32_sort()), ("y".into(), i32_sort())],
            ))
            .with_sum(SumSchema::new(
                "Connection",
                vec![
                    VariantSchema::tag("closed"),
                    VariantSchema::with_payload("open", vec![("socket_fd".into(), i32_sort())]),
                ],
            ));
        assert!(schema.record(&RecordRef::new("Point")).is_some());
        assert!(schema.sum(&SumRef::new("Connection")).is_some());
        assert!(schema.record(&RecordRef::new("Other")).is_none());
    }
}
