use crate::ast::{Ast, Expr, ExprId, Pattern, Stmt, StmtKind, TypeExpr, TypeKind, UnaryOp};
use crate::intern::{Interner, Symbol};
use crate::scalar;

const WIDTH: usize = 80;

impl Ast {
    /// Renders the tree as indented S-expressions, for tests and `biggo parse`.
    pub fn dump(&self, interner: &Interner) -> String {
        let dumper = Dumper {
            ast: self,
            interner,
        };
        let mut out = String::new();
        for stmt in &self.stmts {
            dumper.stmt(stmt).write(&mut out, 0);
            out.push('\n');
        }
        out
    }
}

enum Sexp {
    Atom(String),
    List(Vec<Sexp>),
}

fn atom(text: impl Into<String>) -> Sexp {
    Sexp::Atom(text.into())
}

fn list(head: &str, rest: impl IntoIterator<Item = Sexp>) -> Sexp {
    Sexp::List(std::iter::once(atom(head)).chain(rest).collect())
}

impl Sexp {
    fn flat(&self, out: &mut String) {
        match self {
            Sexp::Atom(text) => out.push_str(text),
            Sexp::List(items) => {
                out.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    item.flat(out);
                }
                out.push(')');
            }
        }
    }

    /// Writes the expression on one line if it fits; otherwise keeps the leading atoms on the
    /// first line and gives every other item its own indented line.
    fn write(&self, out: &mut String, indent: usize) {
        let mut flat = String::new();
        self.flat(&mut flat);
        let Sexp::List(items) = self else {
            out.push_str(&flat);
            return;
        };
        if indent + flat.chars().count() <= WIDTH {
            out.push_str(&flat);
            return;
        }
        let head = items
            .iter()
            .take_while(|item| matches!(item, Sexp::Atom(_)))
            .count()
            .max(1);
        out.push('(');
        for (i, item) in items.iter().enumerate() {
            if i >= head {
                out.push('\n');
                out.push_str(&" ".repeat(indent + 2));
            } else if i > 0 {
                out.push(' ');
            }
            item.write(out, indent + 2);
        }
        out.push(')');
    }
}

struct Dumper<'a> {
    ast: &'a Ast,
    interner: &'a Interner,
}

impl Dumper<'_> {
    fn name(&self, symbol: Symbol) -> &str {
        self.interner.resolve(symbol)
    }

    fn stmt(&self, stmt: &Stmt) -> Sexp {
        match &stmt.kind {
            StmtKind::Let { name, ty, value } => {
                let mut binding = self.name(name.name).to_string();
                if let Some(ty) = ty {
                    binding = format!("{binding}: {}", self.ty(ty));
                }
                list("let", [atom(binding), self.expr(*value)])
            }
            StmtKind::Type { name, ty } => {
                list("type", [atom(self.name(name.name)), atom(self.ty(ty))])
            }
            StmtKind::Fn {
                name,
                params,
                ret,
                body,
            } => {
                let shown: Vec<String> = params
                    .iter()
                    .map(|p| format!("{}: {}", self.name(p.name.name), self.ty(&p.ty)))
                    .collect();
                let mut signature = format!("{}({})", self.name(name.name), shown.join(", "));
                if let Some(ret) = ret {
                    signature = format!("{signature} -> {}", self.ty(ret));
                }
                let defaults = params.iter().filter_map(|param| {
                    let name = atom(self.name(param.name.name));
                    Some(list("default", [name, self.expr(param.default?)]))
                });
                let mut parts = vec![atom(signature)];
                parts.extend(defaults);
                parts.push(self.expr(*body));
                list("fn", parts)
            }
            StmtKind::Import { path } => list("import", [atom(format!("{path:?}"))]),
            StmtKind::Expr(expr) => self.expr(*expr),
        }
    }

    fn expr(&self, id: ExprId) -> Sexp {
        match self.ast.expr(id) {
            Expr::Int(value) => atom(value.to_string()),
            Expr::Float(value) => atom(format!("{value:?}")),
            Expr::Str(value) => atom(format!("{value:?}")),
            Expr::Bool(value) => atom(value.to_string()),
            Expr::Null => atom("null"),
            Expr::Date(date) => atom(format!("@{date}")),
            Expr::Name(name) => atom(self.name(*name)),
            Expr::List(items) => list("list", items.iter().map(|&item| self.expr(item))),
            Expr::Unary { op, operand } => {
                let op = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "not",
                };
                list(op, [self.expr(*operand)])
            }
            Expr::Binary { op, lhs, rhs } => list(op.symbol(), [self.expr(*lhs), self.expr(*rhs)]),
            Expr::Pipe { .. } => {
                // A pipeline nests to the left; print it flat as the source followed by its stages.
                let mut items = Vec::new();
                let mut source = id;
                while let Expr::Pipe { lhs, rhs } = self.ast.expr(source) {
                    items.push(self.expr(*rhs));
                    source = *lhs;
                }
                items.push(self.expr(source));
                items.reverse();
                list("|>", items)
            }
            Expr::Call {
                callee,
                type_args,
                args,
            } => {
                let mut items = vec![self.expr(*callee)];
                if !type_args.is_empty() {
                    items.push(atom(format!("<{}>", self.types(type_args))));
                }
                for arg in args {
                    let value = self.expr(arg.value);
                    items.push(match &arg.name {
                        Some(name) => list(&format!("{}:", self.name(name.name)), [value]),
                        None => value,
                    });
                }
                list("call", items)
            }
            Expr::Field { base, name } => list(".", [self.expr(*base), atom(self.name(name.name))]),
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let parts = [Some(*cond), Some(*then_branch), *else_branch];
                list(
                    "if",
                    parts.into_iter().flatten().map(|part| self.expr(part)),
                )
            }
            Expr::Block(stmts) => list("block", stmts.iter().map(|stmt| self.stmt(stmt))),
            Expr::DateTime(micros) => atom(format!("@{}", scalar::format_datetime(*micros))),
            Expr::Decimal(value) => atom(format!("{}d", scalar::format_decimal(*value))),
            Expr::Record(fields) => {
                let fields = fields.iter().map(|(name, value)| {
                    list(&format!("{}:", self.name(name.name)), [self.expr(*value)])
                });
                list("record", fields)
            }
            Expr::Update { base, fields } => {
                let fields = fields.iter().map(|(name, value)| {
                    Sexp::List(vec![atom(self.name(name.name)), self.expr(*value)])
                });
                list("update", std::iter::once(self.expr(*base)).chain(fields))
            }
            Expr::Map(entries) => {
                let entries = entries
                    .iter()
                    .map(|(key, value)| Sexp::List(vec![self.expr(*key), self.expr(*value)]));
                list("map", entries)
            }
            Expr::Index { base, index } => list("index", [self.expr(*base), self.expr(*index)]),
            Expr::Lambda { params, ret, body } => {
                let params: Vec<String> = params
                    .iter()
                    .map(|param| match &param.ty {
                        Some(ty) => format!("{}: {}", self.name(param.name.name), self.ty(ty)),
                        None => self.name(param.name.name).to_string(),
                    })
                    .collect();
                let mut signature = format!("({})", params.join(", "));
                if let Some(ret) = ret {
                    signature = format!("{signature} -> {}", self.ty(ret));
                }
                list("lambda", [atom(signature), self.expr(*body)])
            }
            Expr::Match { scrutinee, arms } => {
                let arms = arms.iter().map(|arm| {
                    let patterns = arm.patterns.iter().map(|pattern| match pattern {
                        Pattern::Wildcard(_) => atom("_"),
                        Pattern::Value(value) => self.expr(*value),
                    });
                    list(
                        "arm",
                        patterns.chain([self.expr(arm.body)]).collect::<Vec<_>>(),
                    )
                });
                list(
                    "match",
                    std::iter::once(self.expr(*scrutinee))
                        .chain(arms)
                        .collect::<Vec<_>>(),
                )
            }
        }
    }

    fn ty(&self, ty: &TypeExpr) -> String {
        match &ty.kind {
            TypeKind::Named { name, args } if args.is_empty() => self.name(*name).to_string(),
            TypeKind::Named { name, args } => {
                format!("{}<{}>", self.name(*name), self.types(args))
            }
            TypeKind::Record(fields) => {
                let fields: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{}: {}", self.name(f.name.name), self.ty(&f.ty)))
                    .collect();
                format!("{{{}}}", fields.join(", "))
            }
            TypeKind::Nullable(inner) => format!("{}?", self.ty(inner)),
            TypeKind::Fn { params, ret } => match ret {
                Some(ret) => format!("fn({}) -> {}", self.types(params), self.ty(ret)),
                None => format!("fn({})", self.types(params)),
            },
        }
    }

    fn types(&self, types: &[TypeExpr]) -> String {
        let types: Vec<String> = types.iter().map(|ty| self.ty(ty)).collect();
        types.join(", ")
    }
}
