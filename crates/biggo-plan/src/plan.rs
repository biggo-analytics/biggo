use std::any::Any;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::csv::{CsvOptions, ReadOptions};
use crate::expr::{AggCall, Expr, SortKey, WindowCall};
use crate::schema::{Field, Name, Scalar, Schema};

/// A query: a tree of relational operators, each producing a table from its inputs. Plans are
/// immutable and shared, so one table value can feed several pipelines.
#[derive(Debug)]
pub enum Plan {
    Scan(Scan),
    Memory(Memory),
    Filter {
        input: Arc<Plan>,
        predicate: Expr,
    },
    /// Computes each output column from the input row.
    Project {
        input: Arc<Plan>,
        columns: Vec<(Arc<str>, Expr)>,
        schema: Arc<Schema>,
    },
    /// Sorts by `keys`, nulls last. With `fetch`, only the first rows are produced.
    Sort {
        input: Arc<Plan>,
        keys: Vec<SortKey>,
        fetch: Option<usize>,
    },
    Limit {
        input: Arc<Plan>,
        skip: usize,
        fetch: Option<usize>,
    },
    /// The result of `group`. It cannot run on its own; `agg` turns it into an `Aggregate`.
    Group {
        input: Arc<Plan>,
        keys: Vec<(Arc<str>, Expr)>,
    },
    /// One row per distinct combination of `keys`, in order of first appearance. Without keys,
    /// exactly one row.
    Aggregate {
        input: Arc<Plan>,
        keys: Vec<(Arc<str>, Expr)>,
        aggs: Vec<(Arc<str>, AggCall)>,
        schema: Arc<Schema>,
    },
    Join(Join),
    /// Appends one column per function; rows keep their order.
    Window {
        input: Arc<Plan>,
        partition: Vec<Expr>,
        order: Vec<SortKey>,
        funcs: Vec<(Arc<str>, WindowCall)>,
        schema: Arc<Schema>,
    },
    /// The rows of each input in turn. All inputs have the same columns.
    Union {
        inputs: Vec<Arc<Plan>>,
    },
    /// Turns `columns` into rows: each input row gives one row per column, holding the
    /// column's name in `name` and its value in `value`, after the other columns.
    Unpivot {
        input: Arc<Plan>,
        columns: Vec<Arc<str>>,
        name: Arc<str>,
        value: Arc<str>,
        schema: Arc<Schema>,
    },
    /// Splits the string in `column` at `separator` and gives one row per piece.
    Explode {
        input: Arc<Plan>,
        column: Arc<str>,
        separator: Arc<str>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Csv,
    Parquet,
    /// One JSON object per line.
    Json,
    /// The rows of a query on a SQLite database.
    Sqlite,
    /// One sheet of an Excel workbook.
    Excel,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Parquet => "parquet",
            Format::Json => "json",
            Format::Sqlite => "sqlite",
            Format::Excel => "excel",
        }
    }
}

/// Reads a file. Rows that fail `filters` are dropped, and only the columns of `schema` are
/// produced.
#[derive(Clone, Debug)]
pub struct Scan {
    pub format: Format,
    pub path: PathBuf,
    /// The path as written in the program.
    pub display_path: Arc<str>,
    /// For a database, the query whose rows are read.
    pub query: Option<Arc<str>>,
    /// For a CSV file, how it is laid out.
    pub csv: CsvOptions,
    pub read: ReadOptions,
    /// The columns the program declared for the file.
    pub declared: Arc<Schema>,
    pub schema: Arc<Schema>,
    pub filters: Vec<Expr>,
    /// Stop after producing this many rows.
    pub limit: Option<usize>,
}

/// A table held in memory. The engine that created `data` knows its concrete type.
#[derive(Clone)]
pub struct Memory {
    pub schema: Arc<Schema>,
    pub rows: usize,
    pub data: Arc<dyn Any + Send + Sync>,
}

impl fmt::Debug for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Memory({} rows)", self.rows)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    /// Every left row, with nulls where no right row matches.
    Left,
    /// Every row of both sides.
    Full,
    /// Left rows that have a match.
    Semi,
    /// Left rows that have no match.
    Anti,
}

impl JoinKind {
    pub fn name(self) -> &'static str {
        match self {
            JoinKind::Inner => "inner",
            JoinKind::Left => "left",
            JoinKind::Full => "full",
            JoinKind::Semi => "semi",
            JoinKind::Anti => "anti",
        }
    }
}

/// Where a column of a join's output comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum JoinColumn {
    Left(Arc<str>),
    Right(Arc<str>),
    /// A shared key: the left column, or the right one in rows that have no left side.
    Key(Arc<str>, Arc<str>),
}

/// Pairs each left row with the right rows whose keys are equal. Null keys match nothing.
#[derive(Clone, Debug)]
pub struct Join {
    pub left: Arc<Plan>,
    pub right: Arc<Plan>,
    pub kind: JoinKind,
    /// Left and right key expressions, compared pairwise.
    pub on: Vec<(Expr, Expr)>,
    pub columns: Vec<(Arc<str>, JoinColumn)>,
    pub schema: Arc<Schema>,
}

impl Plan {
    pub fn schema(&self) -> Arc<Schema> {
        match self {
            Plan::Scan(scan) => scan.schema.clone(),
            Plan::Memory(memory) => memory.schema.clone(),
            Plan::Filter { input, .. }
            | Plan::Sort { input, .. }
            | Plan::Limit { input, .. }
            | Plan::Group { input, .. }
            | Plan::Explode { input, .. } => input.schema(),
            Plan::Project { schema, .. }
            | Plan::Aggregate { schema, .. }
            | Plan::Window { schema, .. }
            | Plan::Unpivot { schema, .. } => schema.clone(),
            Plan::Join(join) => join.schema.clone(),
            Plan::Union { inputs } => inputs[0].schema(),
        }
    }

    pub fn inputs(&self) -> Vec<&Arc<Plan>> {
        match self {
            Plan::Scan(_) | Plan::Memory(_) => Vec::new(),
            Plan::Filter { input, .. }
            | Plan::Project { input, .. }
            | Plan::Sort { input, .. }
            | Plan::Limit { input, .. }
            | Plan::Group { input, .. }
            | Plan::Aggregate { input, .. }
            | Plan::Window { input, .. }
            | Plan::Unpivot { input, .. }
            | Plan::Explode { input, .. } => vec![input],
            Plan::Join(join) => vec![&join.left, &join.right],
            Plan::Union { inputs } => inputs.iter().collect(),
        }
    }
}

/// A table operation as the type checker leaves it: everything about the plan node is known
/// except its inputs and the values of its parameters, which exist only when the program runs.
#[derive(Clone, Debug)]
pub enum TableOp {
    /// Parameter 0 is the path, which for a file can be a pattern that matches several. For
    /// a database, parameter 1 is the query. For a CSV file, parameters 1 to 5 are the
    /// delimiter, the encoding, whether there is a header, the lines to skip, and the texts
    /// that mean null. For a workbook the last three are the same, after the sheet and the
    /// range, each a string or null.
    Read {
        format: Format,
        schema: Arc<Schema>,
        /// The column that holds the path of the file each row came from, if one does.
        file_column: Option<Arc<str>>,
    },
    Filter {
        predicate: Expr,
    },
    Project {
        columns: Vec<(Arc<str>, Expr)>,
        schema: Arc<Schema>,
    },
    Sort {
        keys: Vec<SortKey>,
    },
    /// Parameter 0 is the number of rows to keep.
    Take,
    /// Parameter 0 is the number of rows to drop.
    Skip,
    Group {
        keys: Vec<(Arc<str>, Expr)>,
    },
    /// The input is a `Group`, or a table to aggregate as a single group.
    Aggregate {
        aggs: Vec<(Arc<str>, AggCall)>,
        schema: Arc<Schema>,
    },
    Join {
        kind: JoinKind,
        on: Vec<(Expr, Expr)>,
        columns: Vec<(Arc<str>, JoinColumn)>,
        schema: Arc<Schema>,
    },
    Window {
        partition: Vec<Expr>,
        order: Vec<SortKey>,
        funcs: Vec<(Arc<str>, WindowCall)>,
        schema: Arc<Schema>,
    },
    Union,
    Unpivot {
        columns: Vec<Arc<str>>,
        name: Arc<str>,
        value: Arc<str>,
        schema: Arc<Schema>,
    },
    Explode {
        column: Arc<str>,
        separator: Arc<str>,
    },
}

impl TableOp {
    /// Builds the plan node. `base_dir` is the directory that relative file paths start from.
    pub fn build(
        &self,
        mut inputs: Vec<Arc<Plan>>,
        params: &[Scalar],
        base_dir: &Path,
    ) -> Result<Plan, String> {
        let bind = |expr: &Expr| expr.bind(params);
        let bind_named = |columns: &[(Arc<str>, Expr)]| -> Vec<(Arc<str>, Expr)> {
            let bound = columns
                .iter()
                .map(|(name, expr)| (name.clone(), bind(expr)));
            bound.collect()
        };
        let bind_keys = |keys: &[SortKey]| -> Vec<SortKey> {
            let bound = keys.iter().map(|key| SortKey {
                expr: bind(&key.expr),
                ..key.clone()
            });
            bound.collect()
        };
        let count = |what: &str| match params.first() {
            Some(Scalar::Int(n)) => usize::try_from(*n)
                .map_err(|_| format!("`{what}` needs a count of zero or more, but got {n}")),
            other => Err(format!("`{what}` needs an int, but got {other:?}")),
        };
        let mut input = || inputs.remove(0);

        Ok(match self {
            TableOp::Read {
                format,
                schema,
                file_column,
            } => {
                let text = |index: usize, what: &str| match params.get(index) {
                    Some(Scalar::Str(text)) => Ok(text.clone()),
                    _ => Err(format!("{what} must be a string")),
                };
                let path = text(0, "the file path")?;
                let mut query = None;
                let mut csv = CsvOptions::default();
                let mut read = ReadOptions {
                    file_column: file_column.clone(),
                    ..ReadOptions::default()
                };
                match format {
                    Format::Sqlite => query = Some(text(1, "the query")?),
                    Format::Csv | Format::Excel => {
                        if *format == Format::Csv {
                            csv.delimiter = CsvOptions::delimiter(&text(1, "the delimiter")?)?;
                            csv.encoding = CsvOptions::encoding(&text(2, "the encoding")?)?;
                        } else {
                            let named = |index: usize| match params.get(index) {
                                Some(Scalar::Str(text)) => Some(text.clone()),
                                _ => None,
                            };
                            read.sheet = named(1);
                            read.range = named(2);
                        }
                        read.header = !matches!(params.get(3), Some(Scalar::Bool(false)));
                        read.skip = match params.get(4) {
                            Some(Scalar::Int(lines)) => usize::try_from(*lines)
                                .map_err(|_| format!("`skip` cannot be negative, found {lines}"))?,
                            _ => 0,
                        };
                        if let Some(Scalar::List(texts)) = params.get(5) {
                            for text in texts.iter() {
                                match text {
                                    Scalar::Str(text) if !text.is_empty() => {
                                        read.nulls.push(text.clone());
                                    }
                                    // An empty field is null whatever the list says.
                                    Scalar::Str(_) => {}
                                    _ => return Err("`nulls` must be a list of strings".into()),
                                }
                            }
                        }
                    }
                    Format::Parquet | Format::Json => {}
                }
                Plan::Scan(Scan {
                    format: *format,
                    path: base_dir.join(&*path),
                    display_path: path,
                    query,
                    csv,
                    read,
                    declared: schema.clone(),
                    schema: schema.clone(),
                    filters: Vec::new(),
                    limit: None,
                })
            }
            TableOp::Filter { predicate } => Plan::Filter {
                input: input(),
                predicate: bind(predicate),
            },
            TableOp::Project { columns, schema } => Plan::Project {
                input: input(),
                columns: bind_named(columns),
                schema: schema.clone(),
            },
            TableOp::Sort { keys } => Plan::Sort {
                input: input(),
                keys: bind_keys(keys),
                fetch: None,
            },
            TableOp::Take => Plan::Limit {
                input: input(),
                skip: 0,
                fetch: Some(count("take")?),
            },
            TableOp::Skip => Plan::Limit {
                input: input(),
                skip: count("skip")?,
                fetch: None,
            },
            TableOp::Group { keys } => Plan::Group {
                input: input(),
                keys: bind_named(keys),
            },
            TableOp::Aggregate { aggs, schema } => {
                let aggs = aggs.iter().map(|(name, call)| {
                    let call = AggCall {
                        func: call.func,
                        arg: call.arg.as_ref().map(bind),
                        arg2: call.arg2.as_ref().map(bind),
                        ty: call.ty,
                    };
                    (name.clone(), call)
                });
                let aggs = aggs.collect();
                let source = input();
                let (input, keys) = match &*source {
                    Plan::Group { input, keys } => (input.clone(), keys.clone()),
                    _ => (source.clone(), Vec::new()),
                };
                Plan::Aggregate {
                    input,
                    keys,
                    aggs,
                    schema: schema.clone(),
                }
            }
            TableOp::Join {
                kind,
                on,
                columns,
                schema,
            } => {
                let left = input();
                let right = input();
                Plan::Join(Join {
                    left,
                    right,
                    kind: *kind,
                    on: on.iter().map(|(l, r)| (bind(l), bind(r))).collect(),
                    columns: columns.clone(),
                    schema: schema.clone(),
                })
            }
            TableOp::Window {
                partition,
                order,
                funcs,
                schema,
            } => {
                let funcs = funcs.iter().map(|(name, call)| {
                    let call = WindowCall {
                        func: call.func,
                        arg: call.arg.as_ref().map(bind),
                        offset: call.offset,
                        ty: call.ty,
                    };
                    (name.clone(), call)
                });
                Plan::Window {
                    funcs: funcs.collect(),
                    input: input(),
                    partition: partition.iter().map(bind).collect(),
                    order: bind_keys(order),
                    schema: schema.clone(),
                }
            }
            TableOp::Union => Plan::Union { inputs },
            TableOp::Unpivot {
                columns,
                name,
                value,
                schema,
            } => Plan::Unpivot {
                input: input(),
                columns: columns.clone(),
                name: name.clone(),
                value: value.clone(),
                schema: schema.clone(),
            },
            TableOp::Explode { column, separator } => Plan::Explode {
                input: input(),
                column: column.clone(),
                separator: separator.clone(),
            },
        })
    }
}

/// Lists the fields that `columns` produce.
pub fn schema_of(columns: &[(Arc<str>, Expr)]) -> Schema {
    let fields = columns
        .iter()
        .map(|(name, expr)| Field::new(name.clone(), expr.ty));
    Schema::new(fields.collect())
}

fn write_list<T>(
    f: &mut fmt::Formatter<'_>,
    items: &[T],
    mut item: impl FnMut(&mut fmt::Formatter<'_>, &T) -> fmt::Result,
) -> fmt::Result {
    for (i, value) in items.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        item(f, value)?;
    }
    Ok(())
}

/// Writes `name = expr`, or just the name for a column passed through unchanged.
fn write_named(f: &mut fmt::Formatter<'_>, name: &str, expr: &Expr) -> fmt::Result {
    if expr.as_column().is_some_and(|column| &**column == name) {
        write!(f, "{}", Name(name))
    } else {
        write!(f, "{} = {expr}", Name(name))
    }
}

impl Plan {
    fn fmt_node(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Plan::Scan(scan) => {
                write!(f, "Scan {} {:?}", scan.format.name(), scan.display_path)?;
                write!(f, "{}{}", scan.csv, scan.read)?;
                if let Some(query) = &scan.query {
                    write!(f, " {query:?}")?;
                }
                f.write_str(": ")?;
                write_list(f, &scan.schema.fields, |f, field| {
                    write!(f, "{}", Name(&field.name))
                })?;
                if !scan.filters.is_empty() {
                    f.write_str(" where ")?;
                    write_list(f, &scan.filters, |f, filter| write!(f, "{filter}"))?;
                }
                if let Some(limit) = scan.limit {
                    write!(f, " limit {limit}")?;
                }
                Ok(())
            }
            Plan::Memory(memory) => write!(f, "Table: {} rows", memory.rows),
            Plan::Filter { predicate, .. } => write!(f, "Filter: {predicate}"),
            Plan::Project { columns, .. } => {
                f.write_str("Project: ")?;
                write_list(f, columns, |f, (name, expr)| write_named(f, name, expr))
            }
            Plan::Sort { keys, fetch, .. } => {
                f.write_str("Sort: ")?;
                write_list(f, keys, |f, key| write!(f, "{key}"))?;
                if let Some(fetch) = fetch {
                    write!(f, " (first {fetch})")?;
                }
                Ok(())
            }
            Plan::Limit { skip, fetch, .. } => {
                f.write_str("Limit:")?;
                if *skip > 0 {
                    write!(f, " skip {skip}")?;
                }
                if let Some(fetch) = fetch {
                    write!(f, " take {fetch}")?;
                }
                Ok(())
            }
            Plan::Group { keys, .. } => {
                f.write_str("Group: ")?;
                write_list(f, keys, |f, (name, expr)| write_named(f, name, expr))
            }
            Plan::Aggregate { keys, aggs, .. } => {
                f.write_str("Aggregate: ")?;
                if !keys.is_empty() {
                    f.write_str("by ")?;
                    write_list(f, keys, |f, (name, expr)| write_named(f, name, expr))?;
                    f.write_str("; ")?;
                }
                write_list(f, aggs, |f, (name, call)| {
                    write!(f, "{} = {call}", Name(name))
                })
            }
            Plan::Join(join) => {
                write!(f, "Join {}: ", join.kind.name())?;
                write_list(f, &join.on, |f, (left, right)| {
                    write!(f, "{left} == {right}")
                })
            }
            Plan::Window {
                partition,
                order,
                funcs,
                ..
            } => {
                f.write_str("Window: ")?;
                if !partition.is_empty() {
                    f.write_str("by ")?;
                    write_list(f, partition, |f, expr| write!(f, "{expr}"))?;
                    f.write_str("; ")?;
                }
                if !order.is_empty() {
                    f.write_str("order ")?;
                    write_list(f, order, |f, key| write!(f, "{key}"))?;
                    f.write_str("; ")?;
                }
                write_list(f, funcs, |f, (name, call)| {
                    write!(f, "{} = {call}", Name(name))
                })
            }
            Plan::Union { .. } => f.write_str("Union"),
            Plan::Unpivot {
                columns,
                name,
                value,
                ..
            } => {
                f.write_str("Unpivot: ")?;
                write_list(f, columns, |f, column| write!(f, "{}", Name(column)))?;
                write!(f, " into {}, {}", Name(name), Name(value))
            }
            Plan::Explode {
                column, separator, ..
            } => write!(f, "Explode: {} at {separator:?}", Name(column)),
        }
    }

    fn fmt_tree(&self, f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
        write!(f, "{:width$}", "", width = depth * 2)?;
        self.fmt_node(f)?;
        writeln!(f)?;
        for input in self.inputs() {
            input.fmt_tree(f, depth + 1)?;
        }
        Ok(())
    }
}

/// Formats the plan as an indented tree, one operator per line, inputs below their consumer.
impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_tree(f, 0)
    }
}
