//! Functions of lists and maps in ordinary code. Many go by a name that tables, strings or
//! aggregates already use: `sort`, `sum`, `contains`. Which is meant shows in the first
//! argument, so that is checked first, and a call on a list or a map is checked here.

use biggo_syntax::ast::ExprId;

use crate::check::{ArgSrc, Call, Cx, FnHint, coerce};
use crate::hir::{Builtin, Expr, ExprKind, ListOp};
use crate::ty::Type;

/// The names that mean one thing for a list and another for a table, a string or a column.
const SHARED: [&str; 17] = [
    "sort", "distinct", "take", "skip", "join", "sum", "min", "max", "mean", "first", "last",
    "any", "all", "contains", "index_of", "reverse", "count",
];

/// The names that only lists and maps have.
const OWN: [&str; 7] = [
    "sort_by", "slice", "flatten", "find", "remove", "merge", "entries",
];

/// What a function takes, for the message about a call with other arguments.
fn takes(name: &str) -> &'static str {
    match name {
        "sort" => "a list, and `desc = true` for largest first",
        "sort_by" => "a list and a function that gives the key",
        "contains" | "index_of" => "a list and a value to look for",
        "join" => "a list of strings and a separator",
        "take" | "skip" => "a list and a number of items",
        "slice" => "a list, a position and a length",
        "flatten" => "one list of lists",
        "any" | "all" | "find" => "a list and a function that gives true or false",
        "count" => {
            "a list and a function that gives true or false; \
             `len(xs)` is the number of all its items"
        }
        "remove" => "a map and a key",
        "merge" => "two maps",
        "entries" => "one map",
        _ => "one list",
    }
}

/// Whether values of a type have an order, and can be told equal: numbers, strings, truth
/// values, dates and times, and any of them or null.
fn ordered(ty: &Type) -> bool {
    matches!(
        ty.split_null().0,
        Type::Int
            | Type::Float
            | Type::Decimal
            | Type::Str
            | Type::Bool
            | Type::Date
            | Type::DateTime
            | Type::Duration
    )
}

impl Cx<'_> {
    /// Checks a call if it is one of this module's: a function that only lists and maps
    /// have, or a shared name called on a list or a map. `None` leaves the call to the
    /// function that the name otherwise means.
    pub(crate) fn list_call(&mut self, call: &Call) -> Option<Expr> {
        let own = OWN.contains(&call.name);
        if !own && !SHARED.contains(&call.name) {
            return None;
        }
        let Some(first) = call.args.first().filter(|arg| arg.name.is_none()) else {
            // Without a first argument, a name that only lists have is still a call of it.
            return own.then(|| {
                let message = format!("`{}` takes {}", call.name, takes(call.name));
                self.error(call.span, message);
                Expr::error(call.span)
            });
        };
        // In a table operation these names keep their meaning for columns.
        if !own && !self.columns.is_empty() {
            return None;
        }
        let subject = self.plain(first.value);
        if !own && !matches!(subject.ty, Type::List(_) | Type::Map(..) | Type::Error) {
            // Not a list: the function that takes the call finds this argument checked.
            self.peeked = Some((first.value, subject));
            return None;
        }
        if subject.ty.is_error() {
            return Some(Expr::error(call.span));
        }
        let checked = self.list_function(call, subject, &call.args[1..]);
        Some(checked.unwrap_or_else(|| Expr::error(call.span)))
    }

    fn list_op(&self, call: &Call, op: ListOp, args: Vec<Expr>, ty: Type) -> Option<Expr> {
        let kind = ExprKind::Builtin(Builtin::List(op), args);
        Some(Expr::new(kind, ty, call.span))
    }

    /// The other arguments of a call, which must number `N` and come without names.
    fn rest<const N: usize>(&mut self, call: &Call, rest: &[ArgSrc]) -> Option<[ExprId; N]> {
        if !self.all_positional(call, rest) {
            return None;
        }
        let values: Vec<ExprId> = rest.iter().map(|arg| arg.value).collect();
        match <[ExprId; N]>::try_from(values) {
            Ok(values) => Some(values),
            Err(_) => {
                self.error(
                    call.span,
                    format!("`{}` takes {}", call.name, takes(call.name)),
                );
                None
            }
        }
    }

    /// The type of the items of `subject`, which must be a list.
    fn items(&mut self, call: &Call, subject: &Expr) -> Option<Type> {
        match &subject.ty {
            Type::List(item) => Some((**item).clone()),
            other => {
                let message = format!("`{}` works on a list, found {other}", call.name);
                self.error(subject.span, message);
                None
            }
        }
    }

    /// The type of the items, which must have an order.
    fn ordered_items(&mut self, call: &Call, subject: &Expr) -> Option<Type> {
        let item = self.items(call, subject)?;
        if !ordered(&item) && item != Type::Null {
            let message = format!(
                "`{}` works on numbers, strings, dates and the like, not on {item}",
                call.name
            );
            self.error(subject.span, message);
            return None;
        }
        Some(item)
    }

    /// An argument that is one item of the list, or a value of another wanted type.
    fn wanted(&mut self, call: &Call, value: ExprId, ty: &Type, what: &str) -> Option<Expr> {
        let value = self.plain(value);
        if value.ty.is_error() {
            return None;
        }
        match coerce(value, ty) {
            Ok(value) => Some(value),
            Err(value) => {
                let wanted = ty.split_null().0;
                let message = format!(
                    "{what} of `{}` must be {wanted}, found {}",
                    call.name, value.ty
                );
                self.error(value.span, message);
                None
            }
        }
    }

    /// A function argument that is called with one item, and its result type.
    fn item_function(
        &mut self,
        call: &Call,
        value: ExprId,
        item: &Type,
        ret: Option<&Type>,
    ) -> Option<(Expr, Type)> {
        let hint = FnHint {
            params: vec![item.clone()],
            ret: ret.cloned(),
        };
        let function = self.plain_hinted(value, Some(hint));
        let found = match &function.ty {
            Type::Fn(found) => found.clone(),
            Type::Error => return None,
            other => {
                let message = format!("`{}` needs a function here, found {other}", call.name);
                self.error(function.span, message);
                return None;
            }
        };
        let fits = found.params.len() == 1
            && found.params[0].ty == *item
            && ret.is_none_or(|ret| found.ret == *ret);
        if !fits {
            let message = match ret {
                Some(ret) => format!(
                    "`{}` calls this function with ({item}) and needs {ret} back, but it is {}",
                    call.name, function.ty
                ),
                None => format!(
                    "`{}` calls this function with ({item}), but it is {}",
                    call.name, function.ty
                ),
            };
            self.error(function.span, message);
            return None;
        }
        let ret = found.ret.clone();
        Some((function, ret))
    }

    fn list_function(&mut self, call: &Call, subject: Expr, rest: &[ArgSrc]) -> Option<Expr> {
        let whole = Type::Int;
        let list_ty = subject.ty.clone();
        match call.name {
            "sort" => {
                self.ordered_items(call, &subject)?;
                // `desc = true` puts the largest first.
                let descending = match rest {
                    [] => Expr::new(ExprKind::Bool(false), Type::Bool, call.span),
                    [arg]
                        if arg
                            .name
                            .is_some_and(|name| &*self.text(name.name) == "desc") =>
                    {
                        self.wanted(call, arg.value, &Type::Bool, "`desc`")?
                    }
                    _ => {
                        self.error(call.span, format!("`sort` takes {}", takes("sort")));
                        return None;
                    }
                };
                self.list_op(call, ListOp::Sort, vec![subject, descending], list_ty)
            }
            "sort_by" => {
                let item = self.items(call, &subject)?;
                let [function] = self.rest(call, rest)?;
                let (function, key) = self.item_function(call, function, &item, None)?;
                if !ordered(&key) {
                    let message = format!(
                        "the keys of `sort_by` are numbers, strings, dates and the like, not {key}"
                    );
                    self.error(function.span, message);
                    return None;
                }
                let keys = ExprKind::Builtin(Builtin::MapList, vec![subject.clone(), function]);
                let keys = Expr::new(keys, Type::List(Box::new(key)), call.span);
                self.list_op(call, ListOp::SortBy, vec![subject, keys], list_ty)
            }
            "sum" | "min" | "max" | "mean" => {
                let item = self.items(call, &subject)?;
                let [] = self.rest(call, rest)?;
                let inner = item.split_null().0.clone();
                let number = matches!(inner, Type::Int | Type::Float | Type::Decimal);
                let (op, ty) = match call.name {
                    "sum" if number || inner == Type::Duration => (ListOp::Sum, inner),
                    "mean" if number => (ListOp::Mean, Type::Float.or_null()),
                    "min" if ordered(&item) => (ListOp::Min, inner.or_null()),
                    "max" if ordered(&item) => (ListOp::Max, inner.or_null()),
                    name => {
                        let needs = if matches!(name, "sum" | "mean") {
                            "numbers"
                        } else {
                            "values with an order"
                        };
                        let message = format!("`{name}` of a list needs {needs}, found {list_ty}");
                        self.error(subject.span, message);
                        return None;
                    }
                };
                self.list_op(call, op, vec![subject], ty)
            }
            "contains" | "index_of" => {
                let item = self.ordered_items(call, &subject)?;
                let [value] = self.rest(call, rest)?;
                let value = self.wanted(call, value, &item.clone().or_null(), "the value")?;
                let (op, ty) = match call.name {
                    "contains" => (ListOp::Contains, Type::Bool),
                    _ => (ListOp::IndexOf, whole.or_null()),
                };
                self.list_op(call, op, vec![subject, value], ty)
            }
            "join" => {
                let item = self.items(call, &subject)?;
                if *item.split_null().0 != Type::Str {
                    let message = format!(
                        "`join` of a list needs strings, found {list_ty}; \
                         `map(xs, fn(x) {{ to_string(x) }})` makes them"
                    );
                    self.error(subject.span, message);
                    return None;
                }
                let [separator] = self.rest(call, rest)?;
                let separator = self.wanted(call, separator, &Type::Str, "the separator")?;
                self.list_op(call, ListOp::Join, vec![subject, separator], Type::Str)
            }
            "reverse" => {
                self.items(call, &subject)?;
                let [] = self.rest(call, rest)?;
                self.list_op(call, ListOp::Reverse, vec![subject], list_ty)
            }
            "distinct" => {
                self.ordered_items(call, &subject)?;
                let [] = self.rest(call, rest)?;
                self.list_op(call, ListOp::Distinct, vec![subject], list_ty)
            }
            "take" | "skip" => {
                self.items(call, &subject)?;
                let [count] = self.rest(call, rest)?;
                let count = self.wanted(call, count, &whole, "the number of items")?;
                let op = if call.name == "take" {
                    ListOp::Take
                } else {
                    ListOp::Skip
                };
                self.list_op(call, op, vec![subject, count], list_ty)
            }
            "slice" => {
                self.items(call, &subject)?;
                let [start, length] = self.rest(call, rest)?;
                let start = self.wanted(call, start, &whole, "the position")?;
                let length = self.wanted(call, length, &whole, "the length")?;
                self.list_op(call, ListOp::Slice, vec![subject, start, length], list_ty)
            }
            "first" | "last" => {
                let item = self.items(call, &subject)?;
                let [] = self.rest(call, rest)?;
                let op = if call.name == "first" {
                    ListOp::First
                } else {
                    ListOp::Last
                };
                self.list_op(call, op, vec![subject], item.or_null())
            }
            "flatten" => {
                let item = self.items(call, &subject)?;
                let [] = self.rest(call, rest)?;
                if !matches!(item, Type::List(_)) {
                    let message = format!("`flatten` works on a list of lists, found {list_ty}");
                    self.error(subject.span, message);
                    return None;
                }
                self.list_op(call, ListOp::Flatten, vec![subject], item)
            }
            "any" | "all" | "find" | "count" => {
                let item = self.items(call, &subject)?;
                let [test] = self.rest(call, rest)?;
                let (test, _) = self.item_function(call, test, &item, Some(&Type::Bool))?;
                let kept = ExprKind::Builtin(Builtin::Filter, vec![subject.clone(), test]);
                let kept = Expr::new(kept, list_ty, call.span);
                match call.name {
                    "any" => self.list_op(call, ListOp::NotEmpty, vec![kept], Type::Bool),
                    "count" => {
                        let kind = ExprKind::Builtin(Builtin::Len, vec![kept]);
                        Some(Expr::new(kind, whole, call.span))
                    }
                    "all" => {
                        self.list_op(call, ListOp::SameLength, vec![kept, subject], Type::Bool)
                    }
                    _ => self.list_op(call, ListOp::First, vec![kept], item.or_null()),
                }
            }
            "remove" | "merge" | "entries" => {
                let Type::Map(key, value) = &list_ty else {
                    let message = format!("`{}` works on a map, found {list_ty}", call.name);
                    self.error(subject.span, message);
                    return None;
                };
                match call.name {
                    "remove" => {
                        let [gone] = self.rest(call, rest)?;
                        let gone = self.wanted(call, gone, key, "the key")?;
                        self.list_op(call, ListOp::Remove, vec![subject, gone], list_ty.clone())
                    }
                    "merge" => {
                        let [other] = self.rest(call, rest)?;
                        let other = self.wanted(call, other, &list_ty, "the second map")?;
                        self.list_op(call, ListOp::Merge, vec![subject, other], list_ty.clone())
                    }
                    _ => {
                        let [] = self.rest(call, rest)?;
                        let fields = vec![
                            ("key".into(), (**key).clone()),
                            ("value".into(), (**value).clone()),
                        ];
                        let entry = Type::Record(std::sync::Arc::new(fields));
                        let ty = Type::List(Box::new(entry));
                        self.list_op(call, ListOp::Entries, vec![subject], ty)
                    }
                }
            }
            name => {
                // A shared name that has no meaning for a map.
                let message = format!("`{name}` does not work on {list_ty}");
                self.error(subject.span, message);
                None
            }
        }
    }
}
