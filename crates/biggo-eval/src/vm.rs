//! The virtual machine that runs bytecode.

use std::cmp::Ordering;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use biggo_plan::{BinaryOp, CsvOptions, Plan, Scalar, scalar};
use biggo_syntax::{Diagnostic, SourceFile, Span};
use biggo_types::hir::{Builtin, Conversion, Program};

use crate::bytecode::{Loop, Op, Proto};
use crate::compile::compile;
use crate::value::{Closure, Decimal, Key, Map, Record, Value};

/// How many calls may be in progress at once before the program is stopped with a "stack
/// overflow" error.
const MAX_FRAMES: usize = 100_000;

/// How many rows of a table `print` shows.
const PREVIEW_ROWS: usize = 50;

/// The longest list that `range` makes.
const MAX_RANGE: i64 = 10_000_000;

/// The most bins a `histogram` can have.
const MAX_BINS: i64 = 1_000_000;

const MICROS_PER_DAY: i64 = 86_400_000_000;

/// The name and text of a source: a file, or a single entry typed at the REPL.
#[derive(Debug)]
pub struct Module {
    pub name: String,
    pub source: String,
    /// The directory that relative file paths in the source start from.
    pub dir: PathBuf,
}

/// An error that stopped the program, located in the module where it happened.
#[derive(Debug)]
pub struct RuntimeError {
    module: Rc<Module>,
    pub span: Span,
    pub message: String,
    /// Set when the error is a failure to write the program's output.
    pub io: Option<io::ErrorKind>,
}

impl RuntimeError {
    /// Formats the error together with the source line it points at.
    pub fn render(&self) -> String {
        let file = SourceFile::new(&self.module.name, &self.module.source);
        file.render(&Diagnostic::new(self.span, self.message.clone()))
    }
}

type Result<T> = std::result::Result<T, Box<RuntimeError>>;

/// A call in progress.
struct Frame {
    proto: Rc<Proto>,
    closure: Option<Rc<Closure>>,
    /// The next instruction.
    pc: usize,
    /// Where the frame's values start on the stack; the arguments come first.
    base: usize,
    /// Whether the called closure sits just under the frame and goes away with it.
    callee_below: bool,
}

/// A loop over a list that is in progress: a call of `map`, `filter`, `each`, or `fold`.
struct LoopState {
    kind: Loop,
    items: Rc<[Value]>,
    function: Value,
    /// The position of the item to call the function with next.
    next: usize,
    /// The results so far of a map, or the items a filter has kept.
    out: Vec<Value>,
    /// The value so far of a fold.
    total: Value,
}

/// Runs modules one after another, keeping their globals and functions. Program output goes
/// to `W`.
pub struct Vm<W> {
    out: W,
    /// Every function of the session, by id.
    protos: Vec<Rc<Proto>>,
    /// Global variables by slot; `None` until the `let` that defines one has run.
    globals: Vec<Option<Value>>,
    stack: Vec<Value>,
    frames: Vec<Frame>,
    /// The loops in progress, innermost last.
    loops: Vec<LoopState>,
    base_dir: PathBuf,
    explain: bool,
    /// Whether scalar functions of single values are computed here where they can be.
    own_scalars: bool,
    /// What `args()` gives: the arguments the program was started with.
    args: Rc<[Value]>,
}

impl<W: Write> Vm<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            protos: Vec::new(),
            globals: Vec::new(),
            stack: Vec::new(),
            frames: Vec::new(),
            loops: Vec::new(),
            base_dir: PathBuf::new(),
            explain: false,
            own_scalars: true,
            args: Rc::new([]),
        }
    }

    pub fn output(&mut self) -> &mut W {
        &mut self.out
    }

    /// Sets the directory that relative file paths start from in a source that is not a
    /// file of its own, and in the file a session starts with.
    pub fn set_base_dir(&mut self, dir: impl Into<PathBuf>) {
        self.base_dir = dir.into();
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Sets the arguments that the program sees as `args()`.
    pub fn set_args(&mut self, args: impl IntoIterator<Item = String>) {
        self.args = args.into_iter().map(|arg| Value::Str(arg.into())).collect();
    }

    /// Whether scalar functions of single values are computed by the virtual machine itself
    /// where it knows how, as they are unless this is turned off, or always by the engine.
    /// The answers are the same; the tests turn it off to check that they are.
    pub fn set_own_scalars(&mut self, own: bool) {
        self.own_scalars = own;
    }

    /// In explain mode no query runs: wherever the program would print, write, or count a
    /// table, the query's plan is printed instead.
    pub fn set_explain(&mut self, explain: bool) {
        self.explain = explain;
    }

    /// Compiles and runs a checked module, and returns the value of its last statement. The
    /// globals it defines stay available to later modules, even if it fails midway.
    pub fn run(&mut self, program: &Program, module: &Rc<Module>) -> Result<Value> {
        self.protos.truncate(program.first_function as usize);
        for function in &program.functions {
            self.protos.push(Rc::new(compile(function, module)));
        }
        self.globals.resize(program.globals as usize, None);
        let main = Rc::new(compile(&program.main, module));
        self.execute(main)
    }

    fn fail<T>(&self, frame: &Frame, message: impl Into<String>) -> Result<T> {
        Err(Box::new(RuntimeError {
            module: frame.proto.module.clone(),
            span: frame.proto.spans[frame.pc - 1],
            message: message.into(),
            io: None,
        }))
    }

    fn pop(&mut self) -> Value {
        self.stack
            .pop()
            .expect("the compiler keeps the stack balanced")
    }

    /// Removes the top `count` values, in the order they were pushed.
    fn pop_many(&mut self, count: u32) -> Vec<Value> {
        self.stack.split_off(self.stack.len() - count as usize)
    }

    fn execute(&mut self, main: Rc<Proto>) -> Result<Value> {
        // An earlier run that ended in an error leaves its frames behind.
        self.stack.clear();
        self.frames.clear();
        self.loops.clear();
        let mut frame = Frame {
            proto: main,
            closure: None,
            pc: 0,
            base: 0,
            callee_below: false,
        };
        // Applies an operator to the two ints on top of the stack, leaving the result in the
        // place of the left one.
        macro_rules! ints {
            (|$a:ident, $b:ident| $result:expr) => {{
                let right = self.stack.pop();
                let (Some(Value::Int($b)), Some(left)) = (right, self.stack.last_mut()) else {
                    return self.fail(&frame, "internal error: expected ints");
                };
                let Value::Int($a) = *left else {
                    return self.fail(&frame, "internal error: expected ints");
                };
                *left = $result;
            }};
        }
        macro_rules! checked {
            ($method:ident) => {
                ints!(|a, b| match a.$method(b) {
                    Some(value) => Value::Int(value),
                    None => return self.fail(&frame, "integer overflow"),
                })
            };
        }
        loop {
            let op = frame.proto.code[frame.pc];
            frame.pc += 1;
            match op {
                Op::Const(index) => self.stack.push(frame.proto.consts[index as usize].clone()),
                Op::Unit => self.stack.push(Value::Unit),
                Op::Null => self.stack.push(Value::Null),
                Op::Bool(value) => self.stack.push(Value::Bool(value)),
                Op::Int(value) => self.stack.push(Value::Int(i64::from(value))),
                Op::Local(slot) => {
                    let value = self.stack[frame.base + slot as usize].clone();
                    self.stack.push(value);
                }
                Op::Capture(index) => {
                    let closure = frame.closure.as_ref().expect("only closures capture");
                    self.stack.push(closure.captures[index as usize].clone());
                }
                Op::This => {
                    let closure = frame
                        .closure
                        .clone()
                        .expect("only closures refer to themselves");
                    self.stack.push(Value::Fn(closure));
                }
                Op::Global(slot, name) => match &self.globals[slot as usize] {
                    Some(value) => self.stack.push(value.clone()),
                    None => {
                        let name = &frame.proto.consts[name as usize];
                        let message = format!("`{name}` is used before it has a value");
                        return self.fail(&frame, message);
                    }
                },
                Op::SetGlobal(slot) => self.globals[slot as usize] = Some(self.pop()),
                Op::Pop => drop(self.pop()),
                Op::Slide(count) => {
                    let top = self.stack.len() - 1;
                    self.stack.swap_remove(top - count as usize);
                    self.stack.truncate(top - count as usize + 1);
                }
                Op::Permute(index) => {
                    let permutation = &frame.proto.permutations[index as usize];
                    let start = self.stack.len() - permutation.len();
                    let mut ordered = vec![Value::Unit; permutation.len()];
                    for (value, position) in self.stack.drain(start..).zip(permutation) {
                        ordered[*position as usize] = value;
                    }
                    self.stack.extend(ordered);
                }

                Op::Neg => {
                    let negated = match self.pop() {
                        Value::Null => Value::Null,
                        Value::Float(value) => Value::Float(-value),
                        Value::Int(value) => match value.checked_neg() {
                            Some(value) => Value::Int(value),
                            None => return self.fail(&frame, "integer overflow"),
                        },
                        Value::Decimal(value) => Value::Decimal(Decimal::new(-value.scaled())),
                        other => {
                            return self
                                .fail(&frame, format!("internal error: cannot negate {other}"));
                        }
                    };
                    self.stack.push(negated);
                }
                Op::Not => {
                    let inverted = match self.pop() {
                        Value::Null => Value::Null,
                        Value::Bool(value) => Value::Bool(!value),
                        other => {
                            return self.fail(&frame, format!("internal error: `not` of {other}"));
                        }
                    };
                    self.stack.push(inverted);
                }
                Op::ToFloat => {
                    let float = match self.pop() {
                        Value::Int(value) => Value::Float(value as f64),
                        other => other,
                    };
                    self.stack.push(float);
                }
                Op::Convert(conversion) => {
                    let converted = match (conversion, self.pop()) {
                        (Conversion::IntToDecimal, Value::Int(value)) => {
                            Value::Decimal(Decimal::new(scalar::decimal_from_int(value)))
                        }
                        (Conversion::DecimalToFloat, Value::Decimal(value)) => {
                            Value::Float(scalar::decimal_to_float(value.scaled()))
                        }
                        (Conversion::DateToDateTime, Value::Date(days)) => {
                            Value::DateTime(i64::from(days) * MICROS_PER_DAY)
                        }
                        (_, other) => other,
                    };
                    self.stack.push(converted);
                }
                Op::Binary(op) => {
                    let right = self.pop();
                    let left = self.pop();
                    match apply(op, &left, &right) {
                        Ok(value) => self.stack.push(value),
                        Err(message) => return self.fail(&frame, message),
                    }
                }
                Op::AddInt => checked!(checked_add),
                Op::SubInt => checked!(checked_sub),
                Op::MulInt => checked!(checked_mul),
                Op::LtInt => ints!(|a, b| Value::Bool(a < b)),
                Op::LeInt => ints!(|a, b| Value::Bool(a <= b)),
                Op::GtInt => ints!(|a, b| Value::Bool(a > b)),
                Op::GeInt => ints!(|a, b| Value::Bool(a >= b)),
                Op::EqInt => ints!(|a, b| Value::Bool(a == b)),
                Op::NeInt => ints!(|a, b| Value::Bool(a != b)),

                Op::Jump(target) => frame.pc = target as usize,
                Op::JumpIfFalse(target) => {
                    if matches!(self.pop(), Value::Bool(false)) {
                        frame.pc = target as usize;
                    }
                }
                Op::JumpIfBool(value, target) => {
                    if matches!(self.stack.last(), Some(Value::Bool(top)) if *top == value) {
                        frame.pc = target as usize;
                    }
                }
                Op::JumpIfNotNull(target) => {
                    if matches!(self.stack.last(), Some(Value::Null)) {
                        self.pop();
                    } else {
                        frame.pc = target as usize;
                    }
                }

                Op::List(count) => {
                    let items = self.pop_many(count);
                    self.stack.push(Value::List(items.into()));
                }
                Op::Record(index) => {
                    let names = frame.proto.records[index as usize].clone();
                    let values = self.pop_many(names.len() as u32);
                    self.stack
                        .push(Value::Record(Rc::new(Record { names, values })));
                }
                Op::Field(index) => {
                    let field = match self.pop() {
                        Value::Record(record) => record.values[index as usize].clone(),
                        // A field of a record that is null is null.
                        Value::Null => Value::Null,
                        other => {
                            return self
                                .fail(&frame, format!("internal error: {other} has no fields"));
                        }
                    };
                    self.stack.push(field);
                }
                Op::Map(count) => {
                    let mut map = Map::default();
                    let mut entries = self.pop_many(2 * count).into_iter();
                    while let (Some(key), Some(value)) = (entries.next(), entries.next()) {
                        let Some(key) = Key::from_value(&key) else {
                            return self.fail(&frame, format!("internal error: {key} as a key"));
                        };
                        map.insert(key, value);
                    }
                    self.stack.push(Value::Map(Rc::new(map)));
                }
                Op::Index => {
                    let index = self.pop();
                    let found = match (self.pop(), &index) {
                        (Value::Null, _) => Value::Null,
                        (Value::List(items), Value::Int(position)) => {
                            // A negative position counts from the end.
                            let len = items.len() as i64;
                            let at = if *position < 0 {
                                position + len
                            } else {
                                *position
                            };
                            match usize::try_from(at).ok().and_then(|at| items.get(at)) {
                                Some(item) => item.clone(),
                                None => {
                                    let message = format!(
                                        "index {position} is out of range for a list of {len} \
                                         items"
                                    );
                                    return self.fail(&frame, message);
                                }
                            }
                        }
                        (Value::Map(map), key) => {
                            let found = Key::from_value(key).and_then(|key| map.get(&key).cloned());
                            found.unwrap_or(Value::Null)
                        }
                        (other, _) => {
                            return self
                                .fail(&frame, format!("internal error: cannot index {other}"));
                        }
                    };
                    self.stack.push(found);
                }
                Op::FromRows(index) => {
                    let schema = &frame.proto.schemas[index as usize];
                    let Value::List(records) = self.pop() else {
                        return self.fail(&frame, "internal error: rows that are not a list");
                    };
                    let mut rows = Vec::with_capacity(records.len());
                    for record in records.iter() {
                        let Value::Record(record) = record else {
                            return self.fail(&frame, "internal error: a row that is no record");
                        };
                        let row: Option<Vec<Scalar>> =
                            record.values.iter().map(Value::to_scalar).collect();
                        let Some(row) = row else {
                            return self
                                .fail(&frame, "internal error: a row of other than scalars");
                        };
                        rows.push(row);
                    }
                    match biggo_exec::from_rows(schema, &rows) {
                        Ok(plan) => self.stack.push(Value::Table(Arc::new(plan))),
                        Err(err) => return self.fail(&frame, err.0),
                    }
                }

                Op::LoopStart(kind) => {
                    let function = self.pop();
                    let total = match kind {
                        Loop::Fold => self.pop(),
                        _ => Value::Unit,
                    };
                    let Value::List(items) = self.pop() else {
                        return self.fail(&frame, "internal error: a loop over other than a list");
                    };
                    self.loops.push(LoopState {
                        kind,
                        items,
                        function,
                        next: 0,
                        out: Vec::new(),
                        total,
                    });
                }
                Op::LoopNext(end) => {
                    let state = self.loops.last_mut().expect("emitted inside a loop");
                    match state.items.get(state.next) {
                        Some(item) => {
                            state.next += 1;
                            self.stack.push(state.function.clone());
                            if state.kind == Loop::Fold {
                                let total = std::mem::replace(&mut state.total, Value::Unit);
                                self.stack.push(total);
                            }
                            self.stack.push(item.clone());
                        }
                        None => frame.pc = end as usize,
                    }
                }
                Op::LoopStep(start) => {
                    let result = self
                        .stack
                        .pop()
                        .expect("the compiler keeps the stack balanced");
                    let state = self.loops.last_mut().expect("emitted inside a loop");
                    match state.kind {
                        Loop::Map => state.out.push(result),
                        Loop::Filter => {
                            if matches!(result, Value::Bool(true)) {
                                state.out.push(state.items[state.next - 1].clone());
                            }
                        }
                        Loop::Each => {}
                        Loop::Fold => state.total = result,
                    }
                    frame.pc = start as usize;
                }
                Op::LoopEnd => {
                    let state = self.loops.pop().expect("emitted inside a loop");
                    self.stack.push(match state.kind {
                        Loop::Map | Loop::Filter => Value::List(state.out.into()),
                        Loop::Each => Value::Unit,
                        Loop::Fold => state.total,
                    });
                }
                Op::Call(id) => {
                    if self.frames.len() == MAX_FRAMES {
                        return self
                            .fail(&frame, "stack overflow: the program recurses too deeply");
                    }
                    let proto = self.protos[id as usize].clone();
                    let callee = Frame {
                        base: self.stack.len() - proto.params as usize,
                        proto,
                        closure: None,
                        pc: 0,
                        callee_below: false,
                    };
                    self.frames.push(std::mem::replace(&mut frame, callee));
                }
                Op::CallValue(args) => {
                    if self.frames.len() == MAX_FRAMES {
                        return self
                            .fail(&frame, "stack overflow: the program recurses too deeply");
                    }
                    let base = self.stack.len() - args as usize;
                    let Value::Fn(closure) = &self.stack[base - 1] else {
                        return self.fail(
                            &frame,
                            "internal error: calling something that is not a function",
                        );
                    };
                    let closure = closure.clone();
                    let callee = Frame {
                        proto: self.protos[closure.function as usize].clone(),
                        closure: Some(closure),
                        pc: 0,
                        base,
                        callee_below: true,
                    };
                    self.frames.push(std::mem::replace(&mut frame, callee));
                }
                Op::Closure(function, captures) => {
                    let captures = self.pop_many(captures);
                    let closure = Closure {
                        function,
                        name: self.protos[function as usize].name.clone(),
                        captures,
                    };
                    self.stack.push(Value::Fn(Rc::new(closure)));
                }
                Op::Return => {
                    let result = self.pop();
                    self.stack
                        .truncate(frame.base - usize::from(frame.callee_below));
                    match self.frames.pop() {
                        Some(caller) => {
                            frame = caller;
                            self.stack.push(result);
                        }
                        None => return Ok(result),
                    }
                }

                Op::Builtin(builtin, args) => {
                    let args = self.pop_many(args);
                    let result = self.builtin(&frame, builtin, args)?;
                    self.stack.push(result);
                }
                Op::Scalar(index) => {
                    let site = &frame.proto.scalars[index as usize];
                    let args = self.pop_many(site.args.len() as u32);
                    // The common cases need no trip through the engine.
                    let own = match self.own_scalars {
                        true => crate::scalars::call(site.func, &args),
                        false => None,
                    };
                    match own {
                        Some(Ok(value)) => {
                            self.stack.push(value);
                            continue;
                        }
                        Some(Err(message)) => return self.fail(&frame, message),
                        None => {}
                    }
                    let mut scalars = Vec::with_capacity(args.len());
                    for (value, ty) in args.iter().zip(&site.args) {
                        let Some(scalar) = value.to_scalar() else {
                            return self
                                .fail(&frame, format!("internal error: {value} is not a scalar"));
                        };
                        scalars.push((scalar, *ty));
                    }
                    match biggo_exec::call_scalar(site.func, &scalars, site.ty) {
                        Ok(value) => self.stack.push(Value::from_scalar(value)),
                        Err(err) => return self.fail(&frame, err.0),
                    }
                }
                Op::Table(index) => {
                    let site = &frame.proto.tables[index as usize];
                    let params = self.pop_many(site.params);
                    let inputs = self.pop_many(site.inputs);
                    let mut scalars = Vec::with_capacity(params.len());
                    for value in &params {
                        let Some(scalar) = value.to_scalar() else {
                            return self
                                .fail(&frame, format!("internal error: {value} is not a scalar"));
                        };
                        scalars.push(scalar);
                    }
                    let mut tables = Vec::with_capacity(inputs.len());
                    for value in inputs {
                        let Value::Table(plan) = value else {
                            return self
                                .fail(&frame, format!("internal error: {value} is not a table"));
                        };
                        tables.push(plan);
                    }
                    match site.op.build(tables, &scalars, &frame.proto.module.dir) {
                        Ok(plan) => self.stack.push(Value::Table(Arc::new(plan))),
                        Err(message) => return self.fail(&frame, message),
                    }
                }
            }
        }
    }

    fn write(&mut self, frame: &Frame, text: &str) -> Result<()> {
        self.out.write_all(text.as_bytes()).map_err(|err| {
            Box::new(RuntimeError {
                module: frame.proto.module.clone(),
                span: frame.proto.spans[frame.pc - 1],
                message: format!("cannot write output: {err}"),
                io: Some(err.kind()),
            })
        })
    }

    /// The text that shows a table: its first rows, or in explain mode its plan.
    fn show_table(&self, frame: &Frame, plan: &Arc<Plan>) -> Result<String> {
        if let Plan::Group { .. } = &**plan {
            return Ok("<grouped table; call agg(...) on it to get rows>\n".to_string());
        }
        if self.explain {
            return Ok(explanation(plan));
        }
        match biggo_exec::format_table(plan, PREVIEW_ROWS) {
            Ok(text) => Ok(text),
            Err(err) => self.fail(frame, err.0),
        }
    }

    /// The rows of a table as records.
    fn rows(&self, frame: &Frame, plan: &Arc<Plan>) -> Result<Vec<Value>> {
        let names: Arc<[Arc<str>]> = plan.schema().names().cloned().collect();
        let rows = match biggo_exec::to_rows(plan) {
            Ok(rows) => rows,
            Err(err) => return self.fail(frame, err.0),
        };
        let records = rows.into_iter().map(|row| {
            let record = Record {
                names: names.clone(),
                values: row.into_iter().map(Value::from_scalar).collect(),
            };
            Value::Record(Rc::new(record))
        });
        Ok(records.collect())
    }

    /// Checks that two tables hold the same rows in the same order.
    fn same_tables(&self, frame: &Frame, left: &Arc<Plan>, right: &Arc<Plan>) -> Result<()> {
        let (left, right) = (self.rows(frame, left)?, self.rows(frame, right)?);
        let differing = left.iter().zip(&right).position(|(l, r)| !l.same(r));
        let message = match differing {
            Some(row) => format!(
                "assertion failed: row {} differs\n  left:  {}\n  right: {}",
                row + 1,
                left[row].repr(),
                right[row].repr()
            ),
            None if left.len() != right.len() => format!(
                "assertion failed: the left table has {} rows but the right one has {}",
                left.len(),
                right.len()
            ),
            None => return Ok(()),
        };
        self.fail(frame, message)
    }

    fn builtin(&mut self, frame: &Frame, builtin: Builtin, args: Vec<Value>) -> Result<Value> {
        let table = |index: usize| match &args[index] {
            Value::Table(plan) => plan.clone(),
            other => unreachable!("the checker passes a table here, not {other}"),
        };
        let text = |index: usize| match &args[index] {
            Value::Str(text) => text.clone(),
            other => unreachable!("the checker passes a string here, not {other}"),
        };
        let int = |index: usize| match &args[index] {
            Value::Int(value) => *value,
            other => unreachable!("the checker passes an int here, not {other}"),
        };
        let path = |index: usize| frame.proto.module.dir.join(&*text(index));
        match builtin {
            Builtin::Print | Builtin::Echo => {
                let mut text = String::new();
                for (index, arg) in args.iter().enumerate() {
                    match arg {
                        // A table takes lines of its own.
                        Value::Table(plan) => {
                            if !text.is_empty() && !text.ends_with('\n') {
                                text.push('\n');
                            }
                            text.push_str(&self.show_table(frame, plan)?);
                        }
                        _ => {
                            if index > 0 && !text.ends_with('\n') {
                                text.push(' ');
                            }
                            match builtin {
                                Builtin::Echo => text.push_str(&arg.repr().to_string()),
                                _ => text.push_str(&arg.to_string()),
                            }
                        }
                    }
                }
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                self.write(frame, &text)?;
                Ok(Value::Unit)
            }
            Builtin::Explain => {
                let text = explanation(&table(0));
                self.write(frame, &text)?;
                Ok(Value::Unit)
            }
            Builtin::WriteCsv
            | Builtin::WriteParquet
            | Builtin::WriteJson
            | Builtin::WriteSqlite => {
                let (plan, path) = (table(0), path(1));
                if self.explain {
                    let text = explanation(&plan);
                    self.write(frame, &text)?;
                    return Ok(Value::Unit);
                }
                if let Some(parent) = path.parent() {
                    // Failing to create the directory shows up as failing to create the file.
                    let _ = std::fs::create_dir_all(parent);
                }
                let written = match builtin {
                    Builtin::WriteCsv => {
                        // The checker has looked at options that are written out; one
                        // that the program computed is looked at here.
                        let delimiter = CsvOptions::delimiter(&text(2));
                        let encoding = CsvOptions::output_encoding(&text(3));
                        let options = match (delimiter, encoding) {
                            (Ok(delimiter), Ok(encoding)) => CsvOptions {
                                delimiter,
                                encoding,
                            },
                            (Err(message), _) | (_, Err(message)) => {
                                return self.fail(frame, message);
                            }
                        };
                        biggo_exec::write_csv(&plan, &path, options)
                    }
                    Builtin::WriteParquet => biggo_exec::write_parquet(&plan, &path),
                    Builtin::WriteJson => biggo_exec::write_json(&plan, &path),
                    _ => biggo_exec::write_sqlite(&plan, &path, &text(2)),
                };
                match written {
                    Ok(()) => Ok(Value::Unit),
                    Err(err) => self.fail(frame, err.0),
                }
            }
            Builtin::Collect => {
                let plan = table(0);
                if self.explain {
                    return Ok(Value::Table(plan));
                }
                match biggo_exec::materialize(&plan) {
                    Ok(memory) => Ok(Value::Table(Arc::new(memory))),
                    Err(err) => self.fail(frame, err.0),
                }
            }
            Builtin::Count => {
                let plan = table(0);
                if self.explain {
                    let text = explanation(&plan);
                    self.write(frame, &text)?;
                    return Ok(Value::Int(0));
                }
                match biggo_exec::count(&plan) {
                    Ok(rows) => Ok(Value::Int(rows as i64)),
                    Err(err) => self.fail(frame, err.0),
                }
            }
            Builtin::ToRows => {
                let plan = table(0);
                if self.explain {
                    let text = explanation(&plan);
                    self.write(frame, &text)?;
                    return Ok(Value::List(Rc::new([])));
                }
                Ok(Value::List(self.rows(frame, &plan)?.into()))
            }
            Builtin::OnlyRow => {
                let plan = table(0);
                if self.explain {
                    // The query does not run, so its row is not known.
                    let text = explanation(&plan);
                    self.write(frame, &text)?;
                    let names: Arc<[Arc<str>]> = plan.schema().names().cloned().collect();
                    let values = vec![Value::Null; names.len()];
                    return Ok(Value::Record(Rc::new(Record { names, values })));
                }
                let mut rows = self.rows(frame, &plan)?;
                match (rows.pop(), rows.is_empty()) {
                    (Some(row), true) => Ok(row),
                    _ => self.fail(frame, "internal error: expected a table of one row"),
                }
            }
            Builtin::Describe | Builtin::Histogram => {
                let plan = table(0);
                if self.explain {
                    let text = explanation(&plan);
                    self.write(frame, &text)?;
                }
                let bins = match builtin {
                    Builtin::Histogram => match int(2) {
                        bins @ 1..=MAX_BINS => bins as usize,
                        bins => {
                            let message = format!(
                                "`histogram` needs between 1 and {MAX_BINS} bins, but got {bins}"
                            );
                            return self.fail(frame, message);
                        }
                    },
                    _ => 0,
                };
                let result = match (builtin, self.explain) {
                    (Builtin::Describe, false) => biggo_exec::describe(&plan),
                    (Builtin::Describe, true) => {
                        biggo_exec::from_rows(&Arc::new(biggo_plan::describe_schema()), &[])
                    }
                    (_, false) => biggo_exec::histogram(&plan, &text(1), bins),
                    (_, true) => {
                        biggo_exec::from_rows(&Arc::new(biggo_plan::histogram_schema()), &[])
                    }
                };
                match result {
                    Ok(summary) => Ok(Value::Table(Arc::new(summary))),
                    Err(err) => self.fail(frame, err.0),
                }
            }
            Builtin::Len => Ok(Value::Int(match &args[0] {
                Value::List(items) => items.len() as i64,
                Value::Map(map) => map.len() as i64,
                other => unreachable!("the checker passes a list or a map here, not {other}"),
            })),
            Builtin::Range => {
                let (start, end) = (int(0), int(1));
                let count = end.saturating_sub(start).max(0);
                if count > MAX_RANGE {
                    let message = format!(
                        "`range` would make a list of {count} numbers; the most is {MAX_RANGE}"
                    );
                    return self.fail(frame, message);
                }
                Ok(Value::List((start..end).map(Value::Int).collect()))
            }
            Builtin::Keys | Builtin::Values => {
                let Value::Map(map) = &args[0] else {
                    unreachable!("the checker passes a map here");
                };
                let entries = map.entries().iter();
                Ok(Value::List(match builtin {
                    Builtin::Keys => entries.map(|(key, _)| key.to_value()).collect(),
                    _ => entries.map(|(_, value)| value.clone()).collect(),
                }))
            }
            Builtin::Put | Builtin::HasKey => {
                let (Value::Map(map), Some(key)) = (&args[0], Key::from_value(&args[1])) else {
                    unreachable!("the checker passes a map and a key here");
                };
                if builtin == Builtin::HasKey {
                    return Ok(Value::Bool(map.get(&key).is_some()));
                }
                let mut map = (**map).clone();
                map.insert(key, args[2].clone());
                Ok(Value::Map(Rc::new(map)))
            }
            Builtin::Args => Ok(Value::List(self.args.clone())),
            Builtin::Split => {
                let (whole, separator) = (text(0), text(1));
                let pieces = biggo_plan::text::split(&whole, &separator).into_iter();
                Ok(Value::List(
                    pieces.map(|piece| Value::Str(piece.into())).collect(),
                ))
            }
            Builtin::Assert => match (&args[0], args.get(1)) {
                (Value::Bool(true), _) => Ok(Value::Unit),
                (_, Some(message)) => self.fail(frame, format!("assertion failed: {message}")),
                (_, None) => self.fail(frame, "assertion failed"),
            },
            Builtin::AssertEq => {
                if let (Value::Table(left), Value::Table(right)) = (&args[0], &args[1]) {
                    // Nothing runs in explain mode, so there are no rows to compare.
                    if !self.explain {
                        self.same_tables(frame, left, right)?;
                    }
                    return Ok(Value::Unit);
                }
                if args[0].same(&args[1]) {
                    return Ok(Value::Unit);
                }
                let message = format!(
                    "assertion failed: the values differ\n  left:  {}\n  right: {}",
                    args[0].repr(),
                    args[1].repr()
                );
                self.fail(frame, message)
            }
            Builtin::MapList
            | Builtin::Filter
            | Builtin::Each
            | Builtin::Fold
            | Builtin::FromRows => {
                unreachable!("{builtin:?} is compiled to instructions of its own")
            }
        }
    }
}

/// Describes a query: the plan as the program built it, and the plan that would run.
fn explanation(plan: &Arc<Plan>) -> String {
    let optimized = biggo_exec::optimize(plan);
    format!(
        "plan:\n{}optimized plan:\n{}\n",
        indent(plan),
        indent(&optimized)
    )
}

fn indent(plan: &Plan) -> String {
    let text = plan.to_string();
    text.lines().map(|line| format!("  {line}\n")).collect()
}

/// Applies an arithmetic or comparison operator, or the last step of `and`/`or`. A null
/// operand makes the result null, except where one operand of `and`/`or` settles it.
fn apply(op: BinaryOp, left: &Value, right: &Value) -> std::result::Result<Value, String> {
    use BinaryOp::*;
    use Value::{Bool, Date, DateTime, Duration, Float, Int, List, Null, Str};
    let overflow = |what: &str| format!("{what} overflow");
    Ok(match (op, left, right) {
        (And, Bool(false), _) | (And, _, Bool(false)) => Bool(false),
        (Or, Bool(true), _) | (Or, _, Bool(true)) => Bool(true),
        (And | Or, Bool(_), Bool(right)) => Bool(*right),
        (_, Null, _) | (_, _, Null) => Null,
        (Add, Int(a), Int(b)) => Int(a.checked_add(*b).ok_or("integer overflow")?),
        (Sub, Int(a), Int(b)) => Int(a.checked_sub(*b).ok_or("integer overflow")?),
        (Mul, Int(a), Int(b)) => Int(a.checked_mul(*b).ok_or("integer overflow")?),
        (Rem, Int(_), Int(0)) => return Err("division by zero".into()),
        // Wrapping only matters for `i64::MIN % -1`, whose true result is 0.
        (Rem, Int(a), Int(b)) => Int(a.wrapping_rem(*b)),
        (Add, Str(a), Str(b)) => Str(format!("{a}{b}").into()),
        (Eq | Ne, Bool(a), Bool(b)) => Bool(compare(op, a.cmp(b))),
        (Eq | Ne | Lt | Le | Gt | Ge, Int(a), Int(b)) => Bool(compare(op, a.cmp(b))),
        (Eq | Ne | Lt | Le | Gt | Ge, Str(a), Str(b)) => Bool(compare(op, a.cmp(b))),
        (Eq | Ne | Lt | Le | Gt | Ge, Date(a), Date(b)) => Bool(compare(op, a.cmp(b))),
        (Eq | Ne | Lt | Le | Gt | Ge, DateTime(a), DateTime(b)) => Bool(compare(op, a.cmp(b))),
        (Eq | Ne | Lt | Le | Gt | Ge, Duration(a), Duration(b)) => Bool(compare(op, a.cmp(b))),
        (_, Value::Decimal(a), Value::Decimal(b)) => decimal_op(op, a.scaled(), b.scaled())?,
        // The time between two moments, and a moment a length of time away.
        (Sub, DateTime(a), DateTime(b)) => {
            Duration(a.checked_sub(*b).ok_or_else(|| overflow("duration"))?)
        }
        (Add, DateTime(a), Duration(b)) | (Add, Duration(b), DateTime(a)) => {
            DateTime(a.checked_add(*b).ok_or_else(|| overflow("datetime"))?)
        }
        (Sub, DateTime(a), Duration(b)) => {
            DateTime(a.checked_sub(*b).ok_or_else(|| overflow("datetime"))?)
        }
        (Add, Duration(a), Duration(b)) => {
            Duration(a.checked_add(*b).ok_or_else(|| overflow("duration"))?)
        }
        (Sub, Duration(a), Duration(b)) => {
            Duration(a.checked_sub(*b).ok_or_else(|| overflow("duration"))?)
        }
        (Add, List(a), List(b)) => List(a.iter().chain(b.iter()).cloned().collect()),
        // True if an item equals the value. Failing that, a null among the items leaves the
        // answer unknown, as it does for a chain of `==` joined by `or`.
        (In, value, List(items)) => {
            let mut unknown = false;
            for item in items.iter() {
                match apply(Eq, value, item)? {
                    Bool(true) => return Ok(Bool(true)),
                    Null => unknown = true,
                    _ => {}
                }
            }
            if unknown { Null } else { Bool(false) }
        }
        (_, Float(a), Float(b)) => float_op(op, *a, *b)?,
        // The checker converts ints that meet floats, except under `/`, which it leaves
        // to do the conversion itself.
        (_, Int(a), Int(b)) => float_op(op, *a as f64, *b as f64)?,
        (_, Int(a), Float(b)) => float_op(op, *a as f64, *b)?,
        (_, Float(a), Int(b)) => float_op(op, *a, *b as f64)?,
        _ => {
            return Err(format!(
                "internal error: cannot apply `{}` to {left} and {right}",
                op.symbol()
            ));
        }
    })
}

fn decimal_op(op: BinaryOp, a: i128, b: i128) -> std::result::Result<Value, String> {
    use BinaryOp::*;
    // A decimal holds 38 digits, six of them after the point.
    let exact = |value: Option<i128>| {
        let fits = value.filter(|value| value.unsigned_abs() < 10u128.pow(38));
        let decimal = fits.map(|value| Value::Decimal(Decimal::new(value)));
        decimal.ok_or_else(|| "decimal overflow".to_string())
    };
    Ok(match op {
        Add => exact(a.checked_add(b))?,
        Sub => exact(a.checked_sub(b))?,
        Mul => exact(scalar::decimal_mul(a, b))?,
        Div | Rem if b == 0 => return Err("division by zero".into()),
        Div => exact(scalar::decimal_div(a, b))?,
        Rem => exact(Some(a % b))?,
        Eq | Ne | Lt | Le | Gt | Ge => Value::Bool(compare(op, a.cmp(&b))),
        And | Or | Coalesce | In | NotIn => {
            return Err("internal error: logic on numbers".into());
        }
    })
}

fn float_op(op: BinaryOp, a: f64, b: f64) -> std::result::Result<Value, String> {
    use BinaryOp::*;
    Ok(match op {
        Add => Value::Float(a + b),
        Sub => Value::Float(a - b),
        Mul => Value::Float(a * b),
        Div => Value::Float(a / b),
        Rem => Value::Float(a % b),
        Eq => Value::Bool(a == b),
        Ne => Value::Bool(a != b),
        Lt => Value::Bool(a < b),
        Le => Value::Bool(a <= b),
        Gt => Value::Bool(a > b),
        Ge => Value::Bool(a >= b),
        And | Or | Coalesce | In | NotIn => {
            return Err("internal error: logic on numbers".into());
        }
    })
}

fn compare(op: BinaryOp, ordering: Ordering) -> bool {
    match op {
        BinaryOp::Eq => ordering.is_eq(),
        BinaryOp::Ne => ordering.is_ne(),
        BinaryOp::Lt => ordering.is_lt(),
        BinaryOp::Le => ordering.is_le(),
        BinaryOp::Gt => ordering.is_gt(),
        BinaryOp::Ge => ordering.is_ge(),
        _ => unreachable!("not a comparison operator"),
    }
}
