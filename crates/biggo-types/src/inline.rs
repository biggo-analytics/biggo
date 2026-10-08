//! A function of the program called on columns. The engine does not call functions: what the
//! call computes is the body of the function with the arguments in place of its parameters,
//! which joins the query as if it had been written out there.

use std::collections::HashMap;

use biggo_syntax::Span;

use crate::check::Cx;
use crate::hir::{Builtin, Callee, Expr, ExprKind, Stmt};

/// How many calls deep a function may reach other functions: more is a function that calls
/// itself, which has no body to write out.
const DEPTH: usize = 64;

impl Cx<'_> {
    /// The body of the function that `call` calls, with the arguments of the call in place
    /// of its parameters. `call` is a call of a declared function. Every part of the
    /// result is placed at the call, where a mistake in it is reported.
    pub(crate) fn inline(&mut self, call: Expr, depth: usize) -> Option<Expr> {
        let Expr { kind, span, .. } = call;
        let ExprKind::Call(Callee::Function(id), args) = kind else {
            unreachable!("only a call of a declared function is written out");
        };
        let signature = self.function_signature(id);
        let name = signature.0;
        let refuse = |cx: &mut Self, why: &str| {
            let message = format!("`{name}` cannot be used on a column: {why}");
            cx.error(span, message);
            None
        };
        if depth > DEPTH {
            return refuse(self, "it calls itself");
        }
        let Some(body) = signature.1 else {
            let why = "it is declared below this line, and a function used on a column \
                       must be declared above";
            return refuse(self, why);
        };
        let mut values: HashMap<u32, Expr> = HashMap::new();
        for arg in args {
            values.insert(arg.param, arg.value);
        }
        let mut body = (*body).clone();
        match self.substitute(&mut body, &mut values, span, depth) {
            Ok(()) => Some(body),
            Err(Some(why)) => refuse(self, why),
            // A function that the body calls has said what is wrong with it.
            Err(None) => None,
        }
    }

    /// Puts the values of `values` in place of the variables that `expr` reads, and the
    /// bodies of the functions it calls on columns in place of the calls. An error is why
    /// the function cannot be written out, unless it has been reported already.
    fn substitute(
        &mut self,
        expr: &mut Expr,
        values: &mut HashMap<u32, Expr>,
        span: Span,
        depth: usize,
    ) -> Result<(), Option<&'static str>> {
        expr.span = span;
        match &mut expr.kind {
            ExprKind::Local(slot) => match values.get(slot) {
                Some(value) => *expr = value.clone(),
                None => return Err(Some("it reads a variable before giving it a value")),
            },
            // A variable of the body is replaced by its value wherever it is read.
            ExprKind::Block(stmts, value) => {
                for stmt in std::mem::take(stmts) {
                    match stmt {
                        Stmt::Let(slot, mut value) => {
                            self.substitute(&mut value, values, span, depth)?;
                            values.insert(slot, value);
                        }
                        Stmt::Global(..) | Stmt::Expr(_) => {
                            return Err(Some("its body does something besides computing a value"));
                        }
                    }
                }
                let Some(mut value) = value.take() else {
                    return Err(Some("it gives no value"));
                };
                self.substitute(&mut value, values, span, depth)?;
                *expr = *value;
            }
            ExprKind::Capture(_) | ExprKind::SelfFn | ExprKind::Closure(..) => {
                return Err(Some("it is, or makes, a function inside a function"));
            }
            // A query takes what does not depend on a row before it reads any row, so the
            // program would stop whatever the rows hold.
            ExprKind::Builtin(Builtin::Fail, _) => {
                return Err(Some("it can stop the program with `fail`"));
            }
            _ => {
                let mut result = Ok(());
                expr.for_each_child_mut(&mut |child| {
                    if result.is_ok() {
                        result = self.substitute(child, values, span, depth);
                    }
                });
                result?;
                // A function that the body calls with a column is written out in turn.
                let calls = matches!(expr.kind, ExprKind::Call(Callee::Function(_), _));
                if calls && expr.find_column_use().is_some() {
                    let call = std::mem::replace(expr, Expr::error(span));
                    *expr = self.inline(call, depth + 1).ok_or(None)?;
                }
            }
        }
        Ok(())
    }
}
