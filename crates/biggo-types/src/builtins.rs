//! Built-in functions on lists, maps, and records, and the assertions that tests are made of.

use biggo_syntax::ast::ExprId;

use crate::check::{Call, Cx, FnHint, coerce, storable, unify};
use crate::hir::{Builtin, Expr, ExprKind};
use crate::ty::Type;

impl Cx<'_> {
    /// Checks a call to one of the functions of this module. Returns `None` if `call.name`
    /// is not one of them.
    pub(crate) fn value_builtin(&mut self, call: &Call) -> Option<Expr> {
        let checked = match call.name {
            "len" => self.builtin_len(call),
            "range" => self.builtin_range(call),
            "map" => self.builtin_map(call),
            "filter" => self.builtin_filter(call),
            "each" => self.builtin_each(call),
            "fold" => self.builtin_fold(call),
            "keys" | "values" => self.builtin_entries(call),
            "put" => self.builtin_put(call),
            "has_key" => self.builtin_has_key(call),
            "to_rows" => self.builtin_to_rows(call),
            "from_rows" => self.builtin_from_rows(call),
            "assert" => self.builtin_assert(call),
            "assert_eq" => self.builtin_assert_eq(call),
            _ => return None,
        };
        Some(checked.unwrap_or_else(|| Expr::error(call.span)))
    }

    /// The arguments of a call that takes exactly `N` of them, all by position. `takes` says
    /// what they are, for the message when the call has others.
    fn exactly<const N: usize>(&mut self, call: &Call, takes: &str) -> Option<[ExprId; N]> {
        if !self.all_positional(call, call.args) {
            return None;
        }
        let values: Vec<ExprId> = call.args.iter().map(|arg| arg.value).collect();
        match <[ExprId; N]>::try_from(values) {
            Ok(values) => Some(values),
            Err(_) => {
                self.error(call.span, format!("`{}` takes {takes}", call.name));
                None
            }
        }
    }

    /// The element type of `list`, which must be a list.
    fn element(&mut self, call: &Call, list: &Expr) -> Option<Type> {
        match &list.ty {
            Type::List(element) => Some((**element).clone()),
            Type::Error => None,
            Type::Table(_) => {
                let message = format!(
                    "`{}` works on a list; `to_rows` gives the rows of a table as one",
                    call.name
                );
                self.error(list.span, message);
                None
            }
            other => {
                let message = format!("`{}` works on a list, found {other}", call.name);
                self.error(list.span, message);
                None
            }
        }
    }

    /// The key and value types of `map`, which must be a map.
    fn entry_types(&mut self, call: &Call, map: &Expr) -> Option<(Type, Type)> {
        match &map.ty {
            Type::Map(key, value) => Some(((**key).clone(), (**value).clone())),
            Type::Error => None,
            other => {
                let message = format!("`{}` works on a map, found {other}", call.name);
                self.error(map.span, message);
                None
            }
        }
    }

    /// Checks an argument that is a function called with values of the types `params`, and
    /// returns it with the type of its result. `ret` is the result type it must have, if
    /// the call fixes one.
    fn function_arg(
        &mut self,
        call: &Call,
        value: ExprId,
        params: &[Type],
        ret: Option<&Type>,
    ) -> Option<(Expr, Type)> {
        let hint = FnHint {
            params: params.to_vec(),
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
        let takes = found.params.iter().map(|param| &param.ty);
        if !takes.eq(params.iter()) {
            let wanted: Vec<String> = params.iter().map(|param| param.to_string()).collect();
            let message = format!(
                "`{}` calls this function with ({}), but it is {}",
                call.name,
                wanted.join(", "),
                function.ty
            );
            self.error(function.span, message);
            return None;
        }
        if let Some(ret) = ret.filter(|ret| **ret != found.ret) {
            let message = format!(
                "`{}` needs a function that returns {ret}, but this one returns {}",
                call.name, found.ret
            );
            self.error(function.span, message);
            return None;
        }
        Some((function, found.ret.clone()))
    }

    fn builtin(&self, call: &Call, builtin: Builtin, args: Vec<Expr>, ty: Type) -> Option<Expr> {
        Some(Expr::new(ExprKind::Builtin(builtin, args), ty, call.span))
    }

    fn builtin_len(&mut self, call: &Call) -> Option<Expr> {
        let [value] = self.exactly(call, "a list or a map")?;
        let value = self.plain(value);
        match &value.ty {
            Type::List(_) | Type::Map(..) => {}
            Type::Error => return None,
            other => {
                let message = match other {
                    Type::Str => "`len` works on a list or a map; \
                                  `length` gives the length of a string"
                        .to_string(),
                    Type::Table(_) => "`len` works on a list or a map; \
                                       `count` gives the number of rows of a table"
                        .to_string(),
                    other => format!("`len` works on a list or a map, found {other}"),
                };
                self.error(value.span, message);
                return None;
            }
        }
        self.builtin(call, Builtin::Len, vec![value], Type::Int)
    }

    /// `range(n)` counts from 0 up to, not including, `n`; `range(a, b)` starts at `a`.
    fn builtin_range(&mut self, call: &Call) -> Option<Expr> {
        if !self.all_positional(call, call.args) {
            return None;
        }
        let (start, end) = match call.args {
            [end] => (None, end),
            [start, end] => (Some(start), end),
            _ => {
                let message = "`range` takes the number to stop before, \
                               or the numbers to start at and to stop before";
                self.error(call.span, message);
                return None;
            }
        };
        let start = match start {
            Some(start) => self.scalar_arg(start.value, Type::Int, "the start of a range")?,
            None => Expr::new(ExprKind::Int(0), Type::Int, call.span),
        };
        let end = self.scalar_arg(end.value, Type::Int, "the end of a range")?;
        let ty = Type::List(Box::new(Type::Int));
        self.builtin(call, Builtin::Range, vec![start, end], ty)
    }

    fn builtin_map(&mut self, call: &Call) -> Option<Expr> {
        let [list, function] = self.exactly(call, "a list and a function")?;
        let list = self.plain(list);
        let element = self.element(call, &list)?;
        let (function, ret) = self.function_arg(call, function, &[element], None)?;
        if !storable(&ret) {
            let message = match ret {
                Type::Unit => "this function gives no value to put in the new list; \
                               `each` calls a function for what it does"
                    .to_string(),
                ret => format!("a list cannot hold values of type {ret}"),
            };
            self.error(function.span, message);
            return None;
        }
        let ty = Type::List(Box::new(ret));
        self.builtin(call, Builtin::MapList, vec![list, function], ty)
    }

    fn builtin_filter(&mut self, call: &Call) -> Option<Expr> {
        let [list, function] = self.exactly(call, "a list and a function")?;
        let list = self.plain(list);
        if matches!(list.ty, Type::Table(_)) {
            let message = "`filter` works on a list; `where` keeps the rows of a table";
            self.error(list.span, message);
            return None;
        }
        let element = self.element(call, &list)?;
        let (function, _) = self.function_arg(call, function, &[element], Some(&Type::Bool))?;
        let ty = list.ty.clone();
        self.builtin(call, Builtin::Filter, vec![list, function], ty)
    }

    fn builtin_each(&mut self, call: &Call) -> Option<Expr> {
        let [list, function] = self.exactly(call, "a list and a function")?;
        let list = self.plain(list);
        let element = self.element(call, &list)?;
        let (function, _) = self.function_arg(call, function, &[element], None)?;
        self.builtin(call, Builtin::Each, vec![list, function], Type::Unit)
    }

    /// `fold(list, start, fn(total, item) { ... })`
    fn builtin_fold(&mut self, call: &Call) -> Option<Expr> {
        let takes = "a list, a starting value, and a function of the value so far and an item";
        let [list, start, function] = self.exactly(call, takes)?;
        let list = self.plain(list);
        let element = self.element(call, &list)?;
        let start = self.plain(start);
        match &start.ty {
            Type::Error => return None,
            Type::Null => {
                let message = "cannot tell which type this `null` has; \
                               put the starting value in a variable with a type first";
                self.error(start.span, message);
                return None;
            }
            ty if !storable(ty) => {
                let message = format!("`fold` cannot build a value of type {ty}");
                self.error(start.span, message);
                return None;
            }
            _ => {}
        }
        let total = start.ty.clone();
        let params = [total.clone(), element];
        let (function, _) = self.function_arg(call, function, &params, Some(&total))?;
        self.builtin(call, Builtin::Fold, vec![list, start, function], total)
    }

    fn builtin_entries(&mut self, call: &Call) -> Option<Expr> {
        let [map] = self.exactly(call, "a map")?;
        let map = self.plain(map);
        let (key, value) = self.entry_types(call, &map)?;
        let (builtin, element) = match call.name {
            "keys" => (Builtin::Keys, key),
            _ => (Builtin::Values, value),
        };
        self.builtin(call, builtin, vec![map], Type::List(Box::new(element)))
    }

    /// Converts `value` to the type the map holds for its keys or its values.
    fn entry_arg(&mut self, call: &Call, value: Expr, ty: &Type, what: &str) -> Option<Expr> {
        if value.ty.is_error() {
            return None;
        }
        match coerce(value, ty) {
            Ok(value) => Some(value),
            Err(value) => {
                let message = format!(
                    "the {what} of this map are {ty}, but `{}` got {}",
                    call.name, value.ty
                );
                self.error(value.span, message);
                None
            }
        }
    }

    fn builtin_put(&mut self, call: &Call) -> Option<Expr> {
        let [map, key, value] = self.exactly(call, "a map, a key, and a value")?;
        let map = self.plain(map);
        let (key_ty, value_ty) = self.entry_types(call, &map)?;
        let key = self.plain(key);
        let key = self.entry_arg(call, key, &key_ty, "keys")?;
        let value = self.plain(value);
        let value = self.entry_arg(call, value, &value_ty, "values")?;
        let ty = map.ty.clone();
        self.builtin(call, Builtin::Put, vec![map, key, value], ty)
    }

    fn builtin_has_key(&mut self, call: &Call) -> Option<Expr> {
        let [map, key] = self.exactly(call, "a map and a key")?;
        let map = self.plain(map);
        let (key_ty, _) = self.entry_types(call, &map)?;
        let key = self.plain(key);
        let key = self.entry_arg(call, key, &key_ty, "keys")?;
        self.builtin(call, Builtin::HasKey, vec![map, key], Type::Bool)
    }

    fn builtin_to_rows(&mut self, call: &Call) -> Option<Expr> {
        let (table, schema) = self.table_arg(call, 0)?;
        if call.args.len() != 1 {
            self.error(call.span, "`to_rows` takes only a table");
            return None;
        }
        let ty = Type::List(Box::new(Type::row_of(&schema)));
        self.builtin(call, Builtin::ToRows, vec![table], ty)
    }

    /// `from_rows(list)`, or `from_rows<{id: int}>(list)` to say which columns the table has
    /// when the list does not.
    fn builtin_from_rows(&mut self, call: &Call) -> Option<Expr> {
        let [rows] = self.exactly(call, "a list of records")?;
        let mut rows = self.plain(rows);
        if rows.ty.is_error() {
            return None;
        }
        match call.type_args {
            [] => {}
            [row] => {
                let row = self.resolve_type(row);
                if row.is_error() {
                    return None;
                }
                let wanted = Type::List(Box::new(row));
                rows = match coerce(rows, &wanted) {
                    Ok(rows) => rows,
                    Err(rows) => {
                        let message = format!("expected {wanted}, found {}", rows.ty);
                        self.error(rows.span, message);
                        return None;
                    }
                };
            }
            [_, extra, ..] => {
                self.error(
                    extra.span,
                    "`from_rows` takes one type argument, the row type",
                );
                return None;
            }
        }
        let row = match &rows.ty {
            Type::List(row) if **row == Type::Null => {
                let message = "cannot tell the columns of an empty list; \
                               give the row type, as in `from_rows<{id: int}>([])`";
                self.error(rows.span, message);
                return None;
            }
            Type::List(row) if matches!(**row, Type::Record(_)) => (**row).clone(),
            other => {
                let message = format!("`from_rows` works on a list of records, found {other}");
                self.error(rows.span, message);
                return None;
            }
        };
        let schema = self.row_schema(&row, rows.span)?;
        if schema.fields.is_empty() {
            self.error(rows.span, "a table needs at least one column");
            return None;
        }
        self.builtin(call, Builtin::FromRows, vec![rows], Type::Table(schema))
    }

    /// `assert(condition)` or `assert(condition, "what went wrong")`
    fn builtin_assert(&mut self, call: &Call) -> Option<Expr> {
        if !self.all_positional(call, call.args) {
            return None;
        }
        let (condition, message) = match call.args {
            [condition] => (condition, None),
            [condition, message] => (condition, Some(message)),
            _ => {
                self.error(
                    call.span,
                    "`assert` takes a condition and, optionally, a message",
                );
                return None;
            }
        };
        let condition = self.plain(condition.value);
        match &condition.ty {
            Type::Bool => {}
            Type::Error => return None,
            Type::Nullable(inner) if **inner == Type::Bool => {
                let message = "this condition can be null; say what null means, \
                               for example with `?? false`";
                self.error(condition.span, message);
                return None;
            }
            other => {
                let message = format!("`assert` checks a bool, found {other}");
                self.error(condition.span, message);
                return None;
            }
        }
        let mut args = vec![condition];
        if let Some(message) = message {
            args.push(self.scalar_arg(message.value, Type::Str, "the message")?);
        }
        self.builtin(call, Builtin::Assert, args, Type::Unit)
    }

    /// `assert_eq(found, expected)` compares two values of one type; two tables are equal
    /// when they have the same columns and the same rows in the same order.
    fn builtin_assert_eq(&mut self, call: &Call) -> Option<Expr> {
        let [left, right] = self.exactly(call, "the two values to compare")?;
        let left = self.plain(left);
        let right = self.plain(right);
        if left.ty.is_error() || right.ty.is_error() {
            return None;
        }
        if let (Type::Table(a), Type::Table(b)) = (&left.ty, &right.ty) {
            // Whether a column may hold null is no part of what the tables hold.
            let same = a.fields.len() == b.fields.len()
                && a.fields
                    .iter()
                    .zip(&b.fields)
                    .all(|(x, y)| x.name == y.name && x.ty.dtype == y.ty.dtype);
            if !same {
                let message =
                    format!("`assert_eq` compares tables with the same columns, found {a} and {b}");
                self.error(call.span, message);
                return None;
            }
            return self.builtin(call, Builtin::AssertEq, vec![left, right], Type::Unit);
        }
        let ty = unify(&left.ty, &right.ty).filter(storable);
        let Some(ty) = ty else {
            let message = format!(
                "`assert_eq` compares two values of one type, found {} and {}",
                left.ty, right.ty
            );
            self.error(call.span, message);
            return None;
        };
        let left = self.converted(left, &ty);
        let right = self.converted(right, &ty);
        if left.ty.is_error() || right.ty.is_error() {
            return None;
        }
        self.builtin(call, Builtin::AssertEq, vec![left, right], Type::Unit)
    }
}
