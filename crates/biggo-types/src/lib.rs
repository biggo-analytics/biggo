//! Name resolution, type checking, and table schema inference. A module that passes is
//! lowered to HIR, the form the bytecode compiler works from.

mod builtins;
mod check;
pub mod hir;
mod lists;
mod tables;
mod ty;
mod verbs;

pub use check::{Checked, Checker, NameKind, Named, Names, Place};
pub use ty::{FnType, Grouped, Param, Type};
pub use verbs::builtin_names;
