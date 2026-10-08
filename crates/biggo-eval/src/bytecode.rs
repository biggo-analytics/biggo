//! The instructions of the virtual machine. It is a stack machine: an instruction takes its
//! operands from the top of the stack and leaves its result there. The local variables of a
//! call live on the same stack, at fixed distances from where the call's frame starts.

use std::rc::Rc;
use std::sync::Arc;

use biggo_plan::{BinaryOp, ColType, ScalarFn, Schema, TableOp};
use biggo_syntax::Span;
use biggo_types::hir::{Builtin, Conversion};

use crate::value::Value;
use crate::vm::Module;

#[derive(Clone, Copy, Debug)]
pub enum Op {
    /// Pushes a constant of the function.
    Const(u32),
    Unit,
    Null,
    Bool(bool),
    Int(i32),
    /// Pushes a copy of the local variable in the given slot.
    Local(u32),
    Capture(u32),
    /// Pushes the closure that is running.
    This,
    /// Pushes a global. The second number is the constant that holds its name.
    Global(u32, u32),
    SetGlobal(u32),
    Pop,
    /// Removes the given number of values from under the top one: the locals of a block
    /// whose value has been computed.
    Slide(u32),
    /// Rearranges the top values: value `i` goes to position `permutation[i]`.
    Permute(u32),

    Neg,
    Not,
    ToFloat,
    Convert(Conversion),
    Binary(BinaryOp),
    // The same on operands known to be ints, with no dispatch on types.
    AddInt,
    SubInt,
    MulInt,
    LtInt,
    LeInt,
    GtInt,
    GeInt,
    EqInt,
    NeInt,

    Jump(u32),
    /// Pops a bool and jumps if it is false.
    JumpIfFalse(u32),
    /// Jumps, keeping the top value, if it is the given bool: the left operand of `and` or
    /// `or` that settles the result.
    JumpIfBool(bool, u32),
    /// Jumps, keeping the top value, unless it is null; pops a null. For `??`.
    JumpIfNotNull(u32),

    List(u32),
    /// Makes a record of the top values; its field names are the given entry of
    /// `Proto::records`.
    Record(u32),
    /// Replaces the record on top by its field at the given position.
    Field(u32),
    /// Makes a map of the given number of keys and values, pushed in turn.
    Map(u32),
    /// Replaces a list and a position, or a map and a key, by what is found there.
    Index,
    /// Makes a table of a list of records; its columns are the given entry of
    /// `Proto::schemas`.
    FromRows(u32),

    /// Starts a loop over a list. The list, the starting value if the loop folds, and the
    /// function to call are on the stack.
    LoopStart(Loop),
    /// Pushes the function and its arguments for the next item, or jumps when no item is
    /// left. A `CallValue` follows.
    LoopNext(u32),
    /// Takes the result of the call into the loop and jumps back to its `LoopNext`.
    LoopStep(u32),
    /// Pushes the value of the loop that has ended.
    LoopEnd,

    /// Calls a function by id; its arguments are on the stack.
    Call(u32),
    /// Calls the closure under the given number of arguments.
    CallValue(u32),
    /// Makes a closure of a function and the given number of captured values.
    Closure(u32, u32),
    Return,

    Builtin(Builtin, u32),
    /// Calls the scalar function described by the given entry of `Proto::scalars`.
    Scalar(u32),
    /// Builds the plan node described by the given entry of `Proto::tables`.
    Table(u32),
}

/// What a loop over a list makes of the results of its function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loop {
    /// A list of the results.
    Map,
    /// A list of the items for which the result is true.
    Filter,
    /// Nothing.
    Each,
    /// The last result, each call taking the result before it.
    Fold,
}

/// A compiled function.
pub struct Proto {
    pub name: Arc<str>,
    pub params: u32,
    pub code: Vec<Op>,
    /// The source location of each instruction, for error messages.
    pub spans: Vec<Span>,
    pub consts: Vec<Value>,
    pub scalars: Vec<ScalarSite>,
    pub tables: Vec<TableSite>,
    pub permutations: Vec<Vec<u32>>,
    /// The field names of the records the function makes.
    pub records: Vec<Arc<[Arc<str>]>>,
    /// The columns of the tables the function makes from records.
    pub schemas: Vec<Arc<Schema>>,
    pub module: Rc<Module>,
}

/// A call of a scalar function: its argument types say which type's null a null is.
pub struct ScalarSite {
    pub func: ScalarFn,
    pub args: Vec<ColType>,
    pub ty: ColType,
}

/// A table operation: the operands on the stack are its input tables, then the values of its
/// parameters.
pub struct TableSite {
    pub op: TableOp,
    pub inputs: u32,
    pub params: u32,
}
