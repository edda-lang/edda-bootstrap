//! `MirProgram`: the program-level container for ADTs, bodies, and interned
//! constants.

use crate::adt::AdtDef;
use crate::arena::IndexVec;
use crate::body::Body;
use crate::constant::Const;
use crate::ids::{AdtId, BodyId, ConstId};

/// The root container for a compiled MIR program.
#[derive(Clone, Debug, Default)]
pub struct MirProgram {
    /// Algebraic data types, indexed by [`AdtId`].
    pub adts: IndexVec<AdtId, AdtDef>,
    /// Function bodies, indexed by [`BodyId`].
    pub bodies: IndexVec<BodyId, Body>,
    /// Interned constants, indexed by [`ConstId`].
    pub consts: IndexVec<ConstId, Const>,
    /// Binary entry body, populated by codegen when this program is the
    /// compilation root of a binary.
    pub entry: Option<BodyId>,
}

impl MirProgram {
    /// Construct an empty program.
    pub fn new() -> Self {
        MirProgram::default()
    }
}
