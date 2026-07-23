//! EUF datatype declaration and slice-length uninterpreted-function caching.
//!
//! Records and sums become Z3 datatypes declared from the
//! [`Schema`](crate::Schema). Slice length is modelled as a per-element-sort
//! uninterpreted function `len_<key>: Array(Int, T) -> Int` declared lazily
//! the first time a slice of that element sort is touched.

use smol_str::SmolStr;

use z3::ast::{Ast, Dynamic};
use z3::{DatatypeAccessor, DatatypeBuilder, DatatypeSort, FuncDecl, Sort as Z3Sort};

use crate::error::TranslationError;
use crate::predicate::Predicate;
use crate::sort::Sort;

use super::{LenKey, Translator};

impl<'ctx, 'schema> Translator<'ctx, 'schema> {
    pub(super) fn translate_field_proj(
        &mut self,
        base: &Predicate,
        field: &crate::sort::FieldRef,
    ) -> Result<Dynamic<'ctx>, TranslationError> {
        let record_name = field.record.name().to_string();
        // Resolve the field's positional index against the schema so the
        // typechecker-supplied [`FieldRef`] is anchored to the same slot as
        // the Z3 datatype's accessor list.
        let field_index = self
            .schema
            .record(&field.record)
            .ok_or_else(|| TranslationError::UnknownTypeName {
                name: SmolStr::new(&record_name),
            })?
            .field_index(field.field.as_str())
            .ok_or_else(|| TranslationError::UnknownMember {
                owner: SmolStr::new(&record_name),
                member: field.field.clone(),
            })?;
        let base_value = self.translate(base)?;
        // Records have exactly one variant in the datatype; the accessor list
        // for that variant maps positional → field.
        let dt = self.record_dt_for(&record_name)?;
        let accessor = &dt.variants[0].accessors[field_index];
        Ok(accessor.apply(&[&base_value as &dyn Ast]))
    }

    pub(super) fn translate_tag_eq(
        &mut self,
        value: &Predicate,
        variant: &crate::sort::VariantRef,
    ) -> Result<Dynamic<'ctx>, TranslationError> {
        let sum_name = variant.sum.name().to_string();
        let (variant_index, payload_free) = {
            let sum_schema = self.schema.sum(&variant.sum).ok_or_else(|| {
                TranslationError::UnknownTypeName {
                    name: SmolStr::new(&sum_name),
                }
            })?;
            let v = sum_schema.variant(variant.variant.as_str()).ok_or_else(|| {
                TranslationError::UnknownMember {
                    owner: SmolStr::new(&sum_name),
                    member: variant.variant.clone(),
                }
            })?;
            let index = sum_schema
                .variant_index(variant.variant.as_str())
                .expect("variant lookup hit");
            (index, v.is_payload_free())
        };
        if !payload_free {
            // Spec §5: payload-bearing variant equality is not in the
            // required-decidable fragment. The typechecker must lower this to
            // a recognizer (`is-variant value`) rather than a `TagEq`; we
            // reject here so the typechecker bug surfaces.
            return Err(TranslationError::Unsupported {
                what: format!(
                    "TagEq on payload-bearing variant `{}.{}` — only payload-free variants are admitted",
                    sum_name, variant.variant
                ),
            });
        }
        let value_dyn = self.translate(value)?;
        // Apply the variant's zero-arg constructor to materialise the constant
        // for that variant, then assert `value == constant`.
        let dt = self.sum_dt_for(&sum_name)?;
        let constructor = &dt.variants[variant_index].constructor;
        let variant_const = constructor.apply(&[]);
        let eq = value_dyn
            ._safe_eq(&variant_const)
            .map_err(|_| TranslationError::SortMismatch {
                expected: Sort::Sum(variant.sum.clone()),
            })?;
        Ok(Dynamic::from_ast(&eq))
    }

    // Lazily declare the Z3 datatype for a record. Cached per name; subsequent
    // calls return the cached `DatatypeSort`.
    pub(super) fn record_dt_for(
        &mut self,
        name: &str,
    ) -> Result<&DatatypeSort<'ctx>, TranslationError> {
        if !self.record_dts.contains_key(name) {
            let schema = self
                .schema
                .record(&crate::sort::RecordRef::new(name))
                .ok_or_else(|| TranslationError::UnknownTypeName {
                    name: SmolStr::new(name),
                })?
                .clone();
            // Sort-translate every field up front; this surfaces nested
            // record / sum dependencies and breaks circular references with
            // an UnknownTypeName error rather than infinite recursion.
            let mut field_sorts: Vec<(SmolStr, Z3Sort<'ctx>)> =
                Vec::with_capacity(schema.fields.len());
            for (field_name, field_sort) in &schema.fields {
                let z3_sort = self.sort_to_z3(field_sort)?;
                field_sorts.push((field_name.clone(), z3_sort));
            }
            let mut builder = DatatypeBuilder::new(self.ctx, schema.name.as_str());
            let constructor_args: Vec<(&str, DatatypeAccessor<'ctx>)> = field_sorts
                .iter()
                .map(|(n, s)| (n.as_str(), DatatypeAccessor::Sort(s.clone())))
                .collect();
            builder = builder.variant(schema.name.as_str(), constructor_args);
            let dt = builder.finish();
            self.record_dts.insert(SmolStr::new(name), dt);
        }
        Ok(self.record_dts.get(name).expect("just inserted"))
    }

    // Lazily declare the Z3 datatype for a sum. Each variant becomes a Z3
    // constructor; payload-free variants take no arguments. Variant
    // distinctness is automatic at the Z3 datatype level.
    pub(super) fn sum_dt_for(
        &mut self,
        name: &str,
    ) -> Result<&DatatypeSort<'ctx>, TranslationError> {
        if !self.sum_dts.contains_key(name) {
            let schema = self
                .schema
                .sum(&crate::sort::SumRef::new(name))
                .ok_or_else(|| TranslationError::UnknownTypeName {
                    name: SmolStr::new(name),
                })?
                .clone();
            // Pre-translate every variant's payload sorts so nested datatypes
            // are declared first.
            let mut variant_payload_sorts: Vec<Vec<(SmolStr, Z3Sort<'ctx>)>> =
                Vec::with_capacity(schema.variants.len());
            for variant in &schema.variants {
                let mut payload_sorts = Vec::with_capacity(variant.payload.len());
                for (field_name, field_sort) in &variant.payload {
                    let z3_sort = self.sort_to_z3(field_sort)?;
                    payload_sorts.push((field_name.clone(), z3_sort));
                }
                variant_payload_sorts.push(payload_sorts);
            }
            let mut builder = DatatypeBuilder::new(self.ctx, schema.name.as_str());
            for (variant, payload_sorts) in
                schema.variants.iter().zip(variant_payload_sorts.iter())
            {
                let args: Vec<(&str, DatatypeAccessor<'ctx>)> = payload_sorts
                    .iter()
                    .map(|(n, s)| (n.as_str(), DatatypeAccessor::Sort(s.clone())))
                    .collect();
                builder = builder.variant(variant.name.as_str(), args);
            }
            let dt = builder.finish();
            self.sum_dts.insert(SmolStr::new(name), dt);
        }
        Ok(self.sum_dts.get(name).expect("just inserted"))
    }

    pub(super) fn len_uf_for(
        &mut self,
        elem_sort: &Sort,
    ) -> Result<&FuncDecl<'ctx>, TranslationError> {
        let key = LenKey(format!("{elem_sort:?}"));
        if !self.len_ufs.contains_key(&key) {
            let elem_z3 = self.sort_to_z3(elem_sort)?;
            let array_sort = Z3Sort::array(self.ctx, &Z3Sort::int(self.ctx), &elem_z3);
            let int_sort = Z3Sort::int(self.ctx);
            // The UF name is suffixed with a per-element-sort key so distinct
            // element sorts get distinct uninterpreted functions per
            // refinement-decidability.md §2 (`len: [T] -> usize` per element).
            let name = format!("len_{}", sanitize_uf_suffix(&key.0));
            let uf = FuncDecl::new(self.ctx, name.as_str(), &[&array_sort], &int_sort);
            self.len_ufs.insert(key.clone(), uf);
        }
        Ok(self.len_ufs.get(&key).expect("just inserted"))
    }
}

// Helper: pull a slice's element sort out of a Predicate that is *supposed* to
// have Slice sort. Used by SliceLen because Z3's array type-erases the element
// sort once translated.
pub(super) fn slice_element_sort(p: &Predicate) -> Result<Sort, TranslationError> {
    match p.sort() {
        Sort::Slice(elem) => Ok(*elem),
        other => Err(TranslationError::SortMismatch {
            expected: Sort::Slice(Box::new(other)),
        }),
    }
}

// Mangle a Debug-form sort into a UF-safe suffix: keep alnum + underscore;
// replace everything else with `_`.
pub(super) fn sanitize_uf_suffix(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}
