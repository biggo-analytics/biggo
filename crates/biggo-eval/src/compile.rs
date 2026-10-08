//! Compiles checked functions to bytecode.

use std::rc::Rc;

use biggo_plan::BinaryOp;
use biggo_syntax::Span;
use biggo_types::Type;
use biggo_types::hir::{Arg, Builtin, Callee, Expr, ExprKind, Function, Stmt};

use crate::bytecode::{Loop, Op, Proto, ScalarSite, TableSite};
use crate::value::{Decimal, Value};
use crate::vm::Module;

pub fn compile(function: &Function, module: &Rc<Module>) -> Proto {
    let proto = Proto {
        name: function.name.clone(),
        params: function.params,
        code: Vec::new(),
        spans: Vec::new(),
        consts: Vec::new(),
        scalars: Vec::new(),
        tables: Vec::new(),
        permutations: Vec::new(),
        records: Vec::new(),
        schemas: Vec::new(),
        module: module.clone(),
    };
    let mut compiler = Compiler {
        proto,
        // The arguments are already on the stack when the function starts.
        depth: function.params,
        slots: (0..function.locals).collect(),
    };
    compiler.expr(&function.body);
    compiler.emit(Op::Return, function.body.span);
    compiler.proto
}

struct Compiler {
    proto: Proto,
    /// How many values the frame holds at the point being compiled.
    depth: u32,
    /// The stack slot of each local variable that has been declared so far.
    slots: Vec<u32>,
}

impl Compiler {
    fn emit(&mut self, op: Op, span: Span) -> usize {
        self.proto.code.push(op);
        self.proto.spans.push(span);
        self.proto.code.len() - 1
    }

    /// Emits an instruction that leaves one more value on the stack.
    fn push(&mut self, op: Op, span: Span) {
        self.emit(op, span);
        self.depth += 1;
    }

    fn constant(&mut self, value: Value) -> u32 {
        self.proto.consts.push(value);
        self.proto.consts.len() as u32 - 1
    }

    /// Makes the jump at `at` land on the next instruction to be emitted.
    fn land(&mut self, at: usize) {
        let here = self.proto.code.len() as u32;
        match &mut self.proto.code[at] {
            Op::Jump(target)
            | Op::JumpIfFalse(target)
            | Op::JumpIfBool(_, target)
            | Op::JumpIfNotNull(target)
            | Op::LoopNext(target) => *target = here,
            other => unreachable!("{other:?} is not a jump"),
        }
    }

    fn exprs<'e>(&mut self, exprs: impl IntoIterator<Item = &'e Expr>) -> u32 {
        let mut count = 0;
        for expr in exprs {
            self.expr(expr);
            count += 1;
        }
        count
    }

    /// Emits code that pushes the arguments of a call, in parameter order.
    fn args(&mut self, args: &[Arg], span: Span) -> u32 {
        let count = self.exprs(args.iter().map(|arg| &arg.value));
        // Arguments are evaluated as written; named ones may belong elsewhere.
        if args
            .iter()
            .enumerate()
            .any(|(index, arg)| arg.param as usize != index)
        {
            self.proto
                .permutations
                .push(args.iter().map(|arg| arg.param).collect());
            self.emit(Op::Permute(self.proto.permutations.len() as u32 - 1), span);
        }
        count
    }

    /// Emits code that leaves the value of `expr` on top of the stack.
    fn expr(&mut self, expr: &Expr) {
        let span = expr.span;
        match &expr.kind {
            ExprKind::Unit => self.push(Op::Unit, span),
            ExprKind::Null => self.push(Op::Null, span),
            ExprKind::Bool(value) => self.push(Op::Bool(*value), span),
            ExprKind::Int(value) => match i32::try_from(*value) {
                Ok(small) => self.push(Op::Int(small), span),
                Err(_) => {
                    let constant = self.constant(Value::Int(*value));
                    self.push(Op::Const(constant), span);
                }
            },
            ExprKind::Float(value) => {
                let constant = self.constant(Value::Float(*value));
                self.push(Op::Const(constant), span);
            }
            ExprKind::Str(value) => {
                let constant = self.constant(Value::Str(value.clone()));
                self.push(Op::Const(constant), span);
            }
            ExprKind::Date(value) => {
                let constant = self.constant(Value::Date(*value));
                self.push(Op::Const(constant), span);
            }
            ExprKind::DateTime(value) => {
                let constant = self.constant(Value::DateTime(*value));
                self.push(Op::Const(constant), span);
            }
            ExprKind::Decimal(value) => {
                let constant = self.constant(Value::Decimal(Decimal::new(*value)));
                self.push(Op::Const(constant), span);
            }
            ExprKind::Local(local) => self.push(Op::Local(self.slots[*local as usize]), span),
            ExprKind::Capture(index) => self.push(Op::Capture(*index), span),
            ExprKind::SelfFn => self.push(Op::This, span),
            ExprKind::Global(id, name) => {
                let name = self.constant(Value::Str(name.clone()));
                self.push(Op::Global(*id, name), span);
            }
            ExprKind::Function(id) => self.push(Op::Closure(*id, 0), span),
            ExprKind::List(items) => {
                let count = self.exprs(items);
                self.depth -= count;
                self.push(Op::List(count), span);
            }
            ExprKind::Neg(operand) => {
                self.expr(operand);
                self.emit(Op::Neg, span);
            }
            ExprKind::Not(operand) => {
                self.expr(operand);
                self.emit(Op::Not, span);
            }
            ExprKind::ToFloat(operand) => {
                self.expr(operand);
                self.emit(Op::ToFloat, span);
            }
            ExprKind::Convert(conversion, operand) => {
                self.expr(operand);
                self.emit(Op::Convert(*conversion), span);
            }
            ExprKind::Record(names, values) => {
                let count = self.exprs(values);
                self.proto.records.push(names.clone());
                self.depth -= count;
                self.push(Op::Record(self.proto.records.len() as u32 - 1), span);
            }
            ExprKind::Field(base, index) => {
                self.expr(base);
                self.emit(Op::Field(*index), span);
            }
            ExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.expr(key);
                    self.expr(value);
                }
                let count = entries.len() as u32;
                self.depth -= 2 * count;
                self.push(Op::Map(count), span);
            }
            ExprKind::Index(base, index) => {
                self.expr(base);
                self.expr(index);
                self.emit(Op::Index, span);
                self.depth -= 1;
            }
            ExprKind::Binary(op, left, right) => self.binary(*op, left, right, span),
            ExprKind::If(cond, then, otherwise) => {
                self.expr(cond);
                let to_else = self.emit(Op::JumpIfFalse(0), span);
                self.depth -= 1;
                self.expr(then);
                match otherwise {
                    Some(otherwise) => {
                        let to_end = self.emit(Op::Jump(0), span);
                        self.land(to_else);
                        self.depth -= 1;
                        self.expr(otherwise);
                        self.land(to_end);
                    }
                    // Without an else branch the `if` has no value, whichever way it goes.
                    None => {
                        self.emit(Op::Pop, span);
                        self.depth -= 1;
                        self.land(to_else);
                        self.push(Op::Unit, span);
                    }
                }
            }
            ExprKind::Block(stmts, value) => {
                let outer = self.depth;
                for stmt in stmts {
                    match stmt {
                        // The value stays where it is computed: that slot is the variable.
                        Stmt::Let(local, value) => {
                            self.expr(value);
                            self.slots[*local as usize] = self.depth - 1;
                        }
                        Stmt::Global(id, value) => {
                            self.expr(value);
                            self.emit(Op::SetGlobal(*id), value.span);
                            self.depth -= 1;
                        }
                        Stmt::Expr(expr) => {
                            self.expr(expr);
                            self.emit(Op::Pop, expr.span);
                            self.depth -= 1;
                        }
                    }
                }
                match value {
                    Some(value) => self.expr(value),
                    None => self.push(Op::Unit, span),
                }
                let locals = self.depth - 1 - outer;
                if locals > 0 {
                    self.emit(Op::Slide(locals), span);
                    self.depth -= locals;
                }
            }
            ExprKind::Call(Callee::Function(id), args) => {
                let count = self.args(args, span);
                self.depth -= count;
                self.push(Op::Call(*id), span);
            }
            ExprKind::Call(Callee::Value(callee), args) => {
                self.expr(callee);
                let count = self.args(args, span);
                self.depth -= count + 1;
                self.push(Op::CallValue(count), span);
            }
            ExprKind::Closure(id, captures) => {
                let count = self.exprs(captures);
                self.depth -= count;
                self.push(Op::Closure(*id, count), span);
            }
            ExprKind::Builtin(
                builtin @ (Builtin::MapList | Builtin::Filter | Builtin::Each | Builtin::Fold),
                args,
            ) => {
                let kind = match builtin {
                    Builtin::MapList => Loop::Map,
                    Builtin::Filter => Loop::Filter,
                    Builtin::Each => Loop::Each,
                    _ => Loop::Fold,
                };
                // The loop runs here, in bytecode, so that the function it calls is called
                // like any other.
                let count = self.exprs(args);
                self.emit(Op::LoopStart(kind), span);
                self.depth -= count;
                let next = self.emit(Op::LoopNext(0), span);
                let passed = if kind == Loop::Fold { 2 } else { 1 };
                self.emit(Op::CallValue(passed), span);
                self.emit(Op::LoopStep(next as u32), span);
                self.land(next);
                self.push(Op::LoopEnd, span);
            }
            ExprKind::Builtin(Builtin::FromRows, args) => {
                let Type::Table(schema) = &expr.ty else {
                    unreachable!("`from_rows` makes a table");
                };
                self.proto.schemas.push(schema.clone());
                let count = self.exprs(args);
                self.depth -= count;
                self.push(Op::FromRows(self.proto.schemas.len() as u32 - 1), span);
            }
            ExprKind::Builtin(builtin, args) => {
                let count = self.exprs(args);
                self.depth -= count;
                self.push(Op::Builtin(*builtin, count), span);
            }
            ExprKind::Scalar(func, args) => {
                let count = self.exprs(args);
                let types = args.iter().map(|arg| {
                    arg.ty
                        .to_col()
                        .expect("the checker only passes column types to scalar functions")
                });
                self.proto.scalars.push(ScalarSite {
                    func: *func,
                    args: types.collect(),
                    ty: expr
                        .ty
                        .to_col()
                        .expect("scalar functions return column types"),
                });
                self.depth -= count;
                self.push(Op::Scalar(self.proto.scalars.len() as u32 - 1), span);
            }
            ExprKind::Table(table) => {
                let inputs = self.exprs(&table.inputs);
                let params = self.exprs(&table.params);
                self.proto.tables.push(TableSite {
                    op: table.op.clone(),
                    inputs,
                    params,
                });
                self.depth -= inputs + params;
                self.push(Op::Table(self.proto.tables.len() as u32 - 1), span);
            }
            ExprKind::Column(_) | ExprKind::Agg(..) | ExprKind::Window(..) | ExprKind::Error => {
                unreachable!("{:?} does not survive type checking", expr.kind)
            }
        }
    }

    fn binary(&mut self, op: BinaryOp, left: &Expr, right: &Expr, span: Span) {
        self.expr(left);
        match op {
            // The right operand is skipped when the left one settles the result.
            BinaryOp::And | BinaryOp::Or => {
                let skip = self.emit(Op::JumpIfBool(op == BinaryOp::Or, 0), span);
                self.expr(right);
                self.emit(Op::Binary(op), span);
                self.depth -= 1;
                self.land(skip);
            }
            BinaryOp::Coalesce => {
                let skip = self.emit(Op::JumpIfNotNull(0), span);
                self.depth -= 1;
                self.expr(right);
                self.land(skip);
            }
            _ => {
                self.expr(right);
                let ints = left.ty == Type::Int && right.ty == Type::Int;
                let op = match op {
                    BinaryOp::Add if ints => Op::AddInt,
                    BinaryOp::Sub if ints => Op::SubInt,
                    BinaryOp::Mul if ints => Op::MulInt,
                    BinaryOp::Lt if ints => Op::LtInt,
                    BinaryOp::Le if ints => Op::LeInt,
                    BinaryOp::Gt if ints => Op::GtInt,
                    BinaryOp::Ge if ints => Op::GeInt,
                    BinaryOp::Eq if ints => Op::EqInt,
                    BinaryOp::Ne if ints => Op::NeInt,
                    _ => Op::Binary(op),
                };
                self.emit(op, span);
                self.depth -= 1;
            }
        }
    }
}
