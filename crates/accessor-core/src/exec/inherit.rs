//! Table inheritance and declarative partitioning.
//!
//! ```sql
//! CREATE TABLE cities (name text, population int);
//! CREATE TABLE capitals (state text) INHERITS (cities);
//! SELECT * FROM cities;            -- the rows of cities and of capitals, as cities' columns
//! SELECT * FROM ONLY cities;       -- cities' own rows
//!
//! CREATE TABLE m (d date, v int) PARTITION BY RANGE (d);
//! CREATE TABLE m2024 PARTITION OF m FOR VALUES FROM ('2024-01-01') TO ('2025-01-01');
//! INSERT INTO m VALUES ('2024-05-01', 1);    -- goes to m2024
//! ```
//!
//! Both are the same mechanism. A child is an ordinary table (own rows, own indexes) with the columns of its parent
//! first. The link is kept in the catalog (`inherit` entries `child|parent`, whose text is the partition bound or empty;
//! `part` entries for the tables that are partitioned, which hold the method and key). Reading a parent reads it and all
//! its descendants, projected on the parent's columns; `UPDATE` and `DELETE` of a parent reach the descendants too;
//! `INSERT` into a partitioned table is sent to the partition whose bound takes the row. `ONLY` leaves the descendants out.
//!
//! Not done: a primary key or unique index across the family, moving a row to another partition when its key changes,
//! `ALTER TABLE ... ATTACH/DETACH PARTITION`, partition pruning in the plan (every partition is read), and
//! `tableoid`.

use std::cmp::Ordering;
use std::sync::atomic::Ordering as AtomicOrdering;

use serde::{Deserialize, Serialize};

use super::select::{Source, SourceData};
use super::QueryResult;
use crate::database::{key, Database};
use crate::error::{Error, Result};
use crate::sql::ast::{BinOp, ColumnDef, Expr, Statement, TableExt};
use crate::storage::Op;
use crate::value::{Row, Value};

/// A partition bound as the parser saves it (expressions as source text).
#[derive(Serialize, Deserialize, Default)]
pub(crate) struct BoundText {
    pub kind: String,
    #[serde(default)]
    pub values: Vec<String>,
    #[serde(default)]
    pub from: Vec<String>,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub modulus: u64,
    #[serde(default)]
    pub remainder: u64,
}

#[derive(Serialize, Deserialize, Default)]
pub(crate) struct PartKey {
    pub method: String,
    pub columns: Vec<String>,
}

thread_local! {
    /// The `WHERE` of the query being planned, for choosing partitions.
    static PRUNE_HINT: std::cell::RefCell<Option<Expr>> = const { std::cell::RefCell::new(None) };
}

/// Sets the condition that partitions are chosen by until the guard is dropped, which puts the earlier one back.
pub(crate) struct PruneGuard(Option<Expr>);

pub(crate) fn prune_guard(filter: Option<Expr>) -> PruneGuard {
    PruneGuard(PRUNE_HINT.with(|h| std::mem::replace(&mut *h.borrow_mut(), filter)))
}

impl Drop for PruneGuard {
    fn drop(&mut self) {
        let earlier = self.0.take();
        PRUNE_HINT.with(|h| *h.borrow_mut() = earlier);
    }
}

/// The `WHERE` of the query being planned, if one is set.
pub(crate) fn current_filter() -> Option<Expr> {
    PRUNE_HINT.with(|h| h.borrow().clone())
}

/// What a query WHERE says of one column: comparisons with values, or a set of values.
enum Fact {
    Cmp(BinOp, Value),
    OneOf(Vec<Value>),
}

fn flip(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Le => BinOp::Ge,
        BinOp::Gt => BinOp::Lt,
        BinOp::Ge => BinOp::Le,
        other => other,
    }
}

/// The facts the conjunction `e` states about column `column` of the table called `names`.
fn facts(e: &Expr, column: &str, names: &[&str], out: &mut Vec<Fact>) {
    let is_col = |x: &Expr| {
        matches!(x, Expr::Column { table, name }
            if name.eq_ignore_ascii_case(column) && table.as_ref().is_none_or(|t| names.iter().any(|n| n.eq_ignore_ascii_case(t))))
    };
    match e {
        Expr::Binary { op: BinOp::And, left, right } => {
            facts(left, column, names, out);
            facts(right, column, names, out);
        }
        Expr::Binary { op: op @ (BinOp::Eq | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge), left, right } => match (&**left, &**right) {
            (l, Expr::Literal(v)) if is_col(l) && !v.is_null() => out.push(Fact::Cmp(*op, v.clone())),
            (Expr::Literal(v), r) if is_col(r) && !v.is_null() => out.push(Fact::Cmp(flip(*op), v.clone())),
            _ => {}
        },
        Expr::Between { expr, low, high, negated: false } if is_col(expr) => {
            if let (Expr::Literal(a), Expr::Literal(b)) = (&**low, &**high) {
                if !a.is_null() && !b.is_null() {
                    out.push(Fact::Cmp(BinOp::Ge, a.clone()));
                    out.push(Fact::Cmp(BinOp::Le, b.clone()));
                }
            }
        }
        Expr::InList { expr, list, negated: false } if is_col(expr) => {
            let vals: Vec<Value> = list.iter().filter_map(|x| if let Expr::Literal(v) = x { Some(v.clone()) } else { None }).collect();
            if vals.len() == list.len() && vals.iter().all(|v| !v.is_null()) {
                out.push(Fact::OneOf(vals));
            }
        }
        _ => {}
    }
}

enum Edge {
    Min,
    Max,
    At(Value),
}

enum Bound {
    Range { from: Vec<Edge>, to: Vec<Edge> },
    List(Vec<Value>),
    Hash { modulus: u64, remainder: u64 },
    Default,
}

fn edge_cmp(v: &Value, e: &Edge) -> Ordering {
    match e {
        Edge::Min => Ordering::Greater,
        Edge::Max => Ordering::Less,
        Edge::At(b) => v.sql_cmp(b).unwrap_or(Ordering::Equal),
    }
}

/// Compares a key with a bound, column by column.
fn tuple_cmp(keys: &[Value], edges: &[Edge]) -> Ordering {
    for (k, e) in keys.iter().zip(edges) {
        match edge_cmp(k, e) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

/// Can a row with key `v` compared as `op` lie in the partition with this bound?
fn may_match(bound: &Bound, op: BinOp, v: &Value) -> bool {
    match bound {
        Bound::Range { from, to } if from.len() == 1 && to.len() == 1 => match op {
            BinOp::Eq => edge_cmp(v, &from[0]) != Ordering::Less && edge_cmp(v, &to[0]) == Ordering::Less,
            BinOp::Lt => edge_cmp(v, &from[0]) == Ordering::Greater,
            BinOp::Le => edge_cmp(v, &from[0]) != Ordering::Less,
            BinOp::Gt | BinOp::Ge => edge_cmp(v, &to[0]) == Ordering::Less,
            _ => true,
        },
        Bound::List(vs) => {
            vs.iter().any(|x| match op {
                BinOp::Eq => x.sql_cmp(v) == Some(Ordering::Equal),
                BinOp::Lt => x.sql_cmp(v) == Some(Ordering::Less),
                BinOp::Le => matches!(x.sql_cmp(v), Some(Ordering::Less | Ordering::Equal)),
                BinOp::Gt => x.sql_cmp(v) == Some(Ordering::Greater),
                BinOp::Ge => matches!(x.sql_cmp(v), Some(Ordering::Greater | Ordering::Equal)),
                _ => true,
            }) || vs.iter().any(Value::is_null)
        }
        Bound::Hash { modulus, remainder } if op == BinOp::Eq => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            v.hash(&mut h);
            *modulus == 0 || h.finish() % modulus == *remainder
        }
        _ => true,
    }
}

impl Database {
    fn inherit_links(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.catalog.iter().filter(|((k, _), _)| k == "inherit").filter_map(|((_, n), v)| {
            let (child, parent) = n.split_once('|')?;
            Some((child, parent, v.as_str()))
        })
    }

    pub(crate) fn children_of(&self, table: &str) -> Vec<String> {
        let t = key(table);
        self.inherit_links().filter(|(_, p, _)| *p == t).map(|(c, _, _)| c.to_string()).collect()
    }

    pub(crate) fn has_children(&self, table: &str) -> bool {
        let t = key(table);
        self.inherit_links().any(|(_, p, _)| p == t)
    }

    /// The table and all that descend from it, the table first.
    pub(crate) fn family(&self, table: &str) -> Vec<String> {
        let mut out = vec![self.table(table).map(|t| t.name().to_string()).unwrap_or_else(|| table.to_string())];
        let mut i = 0;
        while i < out.len() {
            for c in self.children_of(&out[i].clone()) {
                if !out.iter().any(|o| key(o) == c) {
                    out.push(self.table(&c).map(|t| t.name().to_string()).unwrap_or(c));
                }
            }
            i += 1;
        }
        out
    }

    fn part_key(&self, table: &str) -> Option<PartKey> {
        let text = self.catalog.get(&("part".to_string(), key(table)))?;
        serde_json::from_str(text).ok()
    }

    pub(crate) fn is_partitioned(&self, table: &str) -> bool {
        self.catalog.contains_key(&("part".to_string(), key(table)))
    }

    /// `CREATE TABLE ... INHERITS (...)`, `PARTITION BY`, `PARTITION OF`.
    pub(crate) fn exec_create_table_ext(&mut self, ext: &TableExt) -> Result<QueryResult> {
        let Statement::CreateTable { name, columns, primary_key, uniques, foreign_keys, checks } = &ext.create else {
            return Err(Error::runtime("CREATE TABLE expected"));
        };
        let mut parents: Vec<String> = ext.parents.clone();
        let likes = ext.likes.clone();
        if let Some((p, _)) = &ext.partition_of {
            if !self.is_partitioned(p) {
                return Err(Error::catalog(format!("\"{p}\" is not partitioned")));
            }
            parents.push(p.clone());
        }
        let mut defs: Vec<ColumnDef> = Vec::new();
        for p in &parents {
            let pt = self.table_checked(p)?;
            for c in &pt.schema().columns {
                if c.is_calculated() {
                    continue;
                }
                if let Some(old) = defs.iter().find(|d| d.name.eq_ignore_ascii_case(&c.name)) {
                    if old.data_type != c.data_type {
                        return Err(Error::catalog(format!("inherited column \"{}\" has a type conflict", c.name)));
                    }
                    continue;
                }
                defs.push(ColumnDef {
                    name: c.name.clone(),
                    data_type: c.data_type,
                    not_null: c.not_null,
                    primary_key: false,
                    unique: false,
                    autonumber: c.autonumber,
                    default: c.default.clone(),
                    references: None,
                    expression: None,
                    check: None,
                });
            }
        }
        for p in &likes {
            let pt = self.table_checked(p)?;
            for c in &pt.schema().columns {
                if c.is_calculated() || defs.iter().any(|d| d.name.eq_ignore_ascii_case(&c.name)) {
                    continue;
                }
                defs.push(ColumnDef {
                    name: c.name.clone(),
                    data_type: c.data_type,
                    not_null: c.not_null,
                    primary_key: false,
                    unique: false,
                    autonumber: false,
                    default: c.default.clone(),
                    references: None,
                    expression: None,
                    check: None,
                });
            }
        }
        for d in columns {
            match defs.iter().position(|x| x.name.eq_ignore_ascii_case(&d.name)) {
                Some(at) => {
                    if defs[at].data_type != d.data_type {
                        return Err(Error::catalog(format!("column \"{}\" has a type conflict with the inherited column", d.name)));
                    }
                    let not_null = defs[at].not_null || d.not_null;
                    defs[at] = d.clone();
                    defs[at].not_null = not_null;
                }
                None => defs.push(d.clone()),
            }
        }
        let create = Statement::CreateTable {
            name: name.clone(),
            columns: defs,
            primary_key: primary_key.clone(),
            uniques: uniques.clone(),
            foreign_keys: foreign_keys.clone(),
            checks: checks.clone(),
        };
        let ext = ext.clone();
        let parents_copy = parents.clone();
        self.atomically(move |db| {
            db.execute_statement(&create)?;
            for p in &parents_copy {
                let bound = ext.partition_of.as_ref().map(|(_, b)| b.clone()).unwrap_or_default();
                db.apply_all(vec![Op::Catalog { kind: "inherit".into(), name: format!("{}|{}", name, p), value: Some(bound) }])?;
            }
            if let Some((method, cols)) = &ext.partition_by {
                let text = serde_json::to_string(&PartKey { method: method.clone(), columns: cols.clone() }).expect("json");
                for c in cols {
                    if db.table_checked(name)?.schema().column_index(c).is_none() {
                        return Err(Error::catalog(format!("column \"{c}\" named in partition key does not exist")));
                    }
                }
                db.apply_all(vec![Op::Catalog { kind: "part".into(), name: name.clone(), value: Some(text) }])?;
            }
            Ok(QueryResult::default())
        })
    }

    /// `ALTER TABLE parent ATTACH PARTITION child FOR VALUES ...` and `DETACH PARTITION child`.
    pub(crate) fn exec_attach_partition(&mut self, parent: &str, child: &str, bound: Option<&str>) -> Result<QueryResult> {
        let parent_name = self.table_checked(parent)?.name().to_string();
        let child_name = self.table_checked(child)?.name().to_string();
        let link = format!("{}|{}", key(&child_name), key(&parent_name));
        match bound {
            Some(b) => {
                if !self.is_partitioned(&parent_name) {
                    return Err(Error::catalog(format!("\"{parent_name}\" is not partitioned")));
                }
                if self.catalog.contains_key(&("inherit".to_string(), link.clone())) {
                    return Err(Error::catalog(format!("\"{child_name}\" is already a partition of \"{parent_name}\"")));
                }
                let pcols: Vec<String> = self.table_checked(&parent_name)?.schema().columns.iter().map(|c| c.name.to_ascii_lowercase()).collect();
                let ccols: Vec<String> = self.table_checked(&child_name)?.schema().columns.iter().map(|c| c.name.to_ascii_lowercase()).collect();
                if pcols != ccols {
                    return Err(Error::catalog(format!("table \"{child_name}\" does not have the columns of \"{parent_name}\" (same names, same order)")));
                }
                self.apply_all(vec![Op::Catalog { kind: "inherit".into(), name: link, value: Some(b.to_string()) }])?;
            }
            None => {
                if !self.catalog.contains_key(&("inherit".to_string(), link.clone())) {
                    return Err(Error::catalog(format!("\"{child_name}\" is not a partition of \"{parent_name}\"")));
                }
                self.apply_all(vec![Op::Catalog { kind: "inherit".into(), name: link, value: None }])?;
            }
        }
        Ok(QueryResult::default())
    }

    /// The links to forget once `table` is gone.
    pub(crate) fn forget_family_links(&mut self, table: &str) -> Result<()> {
        let t = key(table);
        let links: Vec<String> = self.inherit_links().filter(|(c, p, _)| *c == t || *p == t).map(|(c, p, _)| format!("{c}|{p}")).collect();
        let mut ops: Vec<Op> = links.into_iter().map(|n| Op::Catalog { kind: "inherit".into(), name: n, value: None }).collect();
        if self.is_partitioned(table) {
            ops.push(Op::Catalog { kind: "part".into(), name: table.to_string(), value: None });
        }
        // policies and the security switch of the table go with it
        let prefix = format!("{t}|");
        for (k, n) in self.catalog.keys() {
            if (k == "policy" && n.starts_with(&prefix)) || (k == "rls" && *n == t) {
                ops.push(Op::Catalog { kind: k.clone(), name: n.clone(), value: None });
            }
        }
        if !ops.is_empty() {
            self.apply_all(ops)?;
        }
        Ok(())
    }

    /// `CREATE TABLE name AS query`: the columns take the names of the query's, and the type of their first value.
    pub(crate) fn exec_create_table_as(&mut self, name: &str, query: &crate::sql::ast::Select, with_data: bool) -> Result<QueryResult> {
        let mut probe = query.clone();
        let res = self.run_select(&mut probe)?;
        let columns: Vec<ColumnDef> = res
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| ColumnDef {
                name: c.clone(),
                data_type: res.rows.iter().find_map(|r| r[i].data_type()).unwrap_or(crate::value::DataType::Text),
                not_null: false,
                primary_key: false,
                unique: false,
                autonumber: false,
                default: None,
                references: None,
                expression: None,
                check: None,
            })
            .collect();
        let create = Statement::CreateTable {
            name: name.to_string(),
            columns,
            primary_key: Vec::new(),
            uniques: Vec::new(),
            foreign_keys: Vec::new(),
            checks: Vec::new(),
        };
        let count = res.rows.len();
        let rows = res.rows;
        self.atomically(|db| {
            db.execute_statement(&create)?;
            if with_data && !rows.is_empty() {
                let lits = rows.into_iter().map(|r| r.into_iter().map(crate::sql::ast::Expr::Literal).collect()).collect();
                db.execute_statement(&Statement::Insert {
                    table: name.to_string(),
                    columns: None,
                    source: crate::sql::ast::InsertSource::Values(lits),
                })?;
            }
            Ok(QueryResult::affected(if with_data { count } else { 0 }))
        })
    }

    /// `DROP TABLE a, b [CASCADE]`.
    pub(crate) fn exec_drop_tables(&mut self, names: &[String], cascade: bool, if_exists: bool) -> Result<QueryResult> {
        if !cascade && names.len() > 1 {
            // the tables go as one: a table that another one in the list refers to is dropped after it
            let mut pending: Vec<&String> = Vec::new();
            let mut missing: Option<&String> = None;
            for n in names {
                if self.table(n).is_none() {
                    // as in MySQL, the tables that exist are dropped, and the one that does not is reported
                    missing = missing.or(Some(n));
                    continue;
                }
                pending.push(n);
            }
            while !pending.is_empty() {
                let mut later = Vec::new();
                let mut first_error = None;
                for n in &pending {
                    if let Err(e) = self.exec_drop_table(n) {
                        first_error.get_or_insert(e);
                        later.push(*n);
                    }
                }
                if later.len() == pending.len() {
                    return Err(first_error.expect("a drop failed"));
                }
                pending = later;
            }
            return match missing {
                Some(n) if !if_exists => Err(Error::catalog(format!("the table '{n}' does not exist"))),
                _ => Ok(QueryResult::default()),
            };
        }
        for n in names {
            if self.table(n).is_none() {
                if if_exists {
                    continue;
                }
                return Err(Error::catalog(format!("the table '{n}' does not exist")));
            }
            if cascade {
                self.drop_cascade(n)?;
            } else {
                self.exec_drop_table(n)?;
            }
        }
        Ok(QueryResult::default())
    }

    fn views_using(&self, table: &str) -> Vec<String> {
        self.view_names()
            .into_iter()
            .filter(|v| {
                self.view_sql(v)
                    .is_some_and(|sql| sql.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| w.eq_ignore_ascii_case(table)))
            })
            .map(String::from)
            .collect()
    }

    fn drop_cascade(&mut self, table: &str) -> Result<()> {
        let real = self.table_checked(table)?.name().to_string();
        for child in self.children_of(&real) {
            if self.table(&child).is_some() {
                self.drop_cascade(&child)?;
            }
        }
        for v in self.views_using(&real) {
            if self.view_sql(&v).is_some() {
                self.drop_view_cascade(&v)?;
            }
        }
        // foreign keys of other tables that point here
        let mut ops = Vec::new();
        for t in self.table_names() {
            if t.eq_ignore_ascii_case(&real) {
                continue;
            }
            if let Some(tb) = self.table(t) {
                for fk in &tb.schema().foreign_keys {
                    if fk.ref_table.eq_ignore_ascii_case(&real) {
                        ops.push(Op::DropForeignKey { table: tb.name().to_string(), name: fk.name.clone() });
                    }
                }
            }
        }
        if !ops.is_empty() {
            self.apply_all(ops)?;
        }
        self.exec_drop_table(&real)?;
        Ok(())
    }

    pub(crate) fn drop_view_cascade(&mut self, view: &str) -> Result<()> {
        for v in self.views_using(view) {
            if !v.eq_ignore_ascii_case(view) && self.view_sql(&v).is_some() {
                self.drop_view_cascade(&v)?;
            }
        }
        self.exec_drop_view(view)?;
        Ok(())
    }

    /// The rows of `table` and of its descendants, in the columns of `table`.
    pub(crate) fn family_source(&self, table: &str, alias: String, depth: u32) -> Result<Source<'_>> {
        let hint = PRUNE_HINT.with(|h| h.borrow().clone());
        let (members, total) = self.pruned_family(table, hint.as_ref(), &[table, &alias]);
        if members.len() < total {
            self.note(depth, || format!("scan {table}: {} of {total} tables (the others cannot hold a matching row)", members.len()));
        }
        let parent = self.table_checked(&members[0])?;
        let names: Vec<String> = parent.schema().columns.iter().map(|c| c.name.clone()).collect();
        let mut rows: Vec<Row> = Vec::new();
        for m in &members {
            let t = self.table_checked(m)?;
            self.require_privilege(m, "select")?;
            let map: Vec<Option<usize>> = names.iter().map(|n| t.schema().column_index(n)).collect();
            for (_, row) in t.iter() {
                rows.push(map.iter().map(|i| i.map_or(Value::Null, |i| row[i].clone())).collect());
            }
        }
        Ok(Source { keep: None, intrinsic: Vec::new(), data: SourceData::Rows(rows), alias, names })
    }

    pub(crate) fn dml_members(&self, table: &str, filter: Option<&Expr>) -> Vec<String> {
        if self.only_dml.load(AtomicOrdering::Relaxed) || !self.has_children(table) {
            vec![table.to_string()]
        } else {
            self.pruned_family(table, filter, &[table]).0
        }
    }

    /// The table and its descendants, leaving out the partitions that `filter` cannot take a row from; with the
    /// number of tables there would have been. Only the first column of a partition key is looked at, and only
    /// comparisons with constants, `BETWEEN` and `IN`, joined by `AND`.
    pub(crate) fn pruned_family(&self, table: &str, filter: Option<&Expr>, names: &[&str]) -> (Vec<String>, usize) {
        let all = self.family(table);
        let total = all.len();
        let Some(filter) = filter.filter(|_| self.optimizer_enabled()) else { return (all, total) };
        let mut out = Vec::new();
        self.prune_walk(table, filter, names, &mut out);
        (out, total)
    }

    fn prune_walk(&self, table: &str, filter: &Expr, names: &[&str], out: &mut Vec<String>) {
        out.push(self.table(table).map(|t| t.name().to_string()).unwrap_or_else(|| table.to_string()));
        let bounds = match self.part_key(table) {
            Some(pk) if pk.columns.len() == 1 => self.bounds_of(table).ok().map(|b| (pk, b)),
            _ => None,
        };
        for child in self.children_of(table) {
            let keep = match &bounds {
                Some((pk, bs)) => match bs.iter().find(|(c, _)| *c == child) {
                    Some((_, b)) => self.bound_may_match(table, &pk.columns[0], b, filter, names),
                    None => true,
                },
                None => true,
            };
            if keep {
                self.prune_walk(&child, filter, names, out);
            }
        }
    }

    fn bound_may_match(&self, table: &str, column: &str, bound: &Bound, filter: &Expr, names: &[&str]) -> bool {
        let mut fs = Vec::new();
        facts(filter, column, names, &mut fs);
        let Some(col_type) = self.table(table).and_then(|t| t.schema().column_index(column).map(|i| t.schema().columns[i].data_type)) else {
            return true;
        };
        let coerce = |v: &Value| v.coerce_to(col_type).ok();
        for f in &fs {
            let ok = match f {
                Fact::Cmp(op, v) => match coerce(v) {
                    Some(v) => may_match(bound, *op, &v),
                    None => true,
                },
                Fact::OneOf(vs) => {
                    let cs: Option<Vec<Value>> = vs.iter().map(coerce).collect();
                    match cs {
                        Some(cs) => cs.iter().any(|v| may_match(bound, BinOp::Eq, v)),
                        None => true,
                    }
                }
            };
            if !ok {
                return false;
            }
        }
        true
    }

    fn bounds_of(&self, parent: &str) -> Result<Vec<(String, Bound)>> {
        let p = key(parent);
        let mut out = Vec::new();
        for (child, par, text) in self.inherit_links() {
            if par != p || text.is_empty() {
                continue;
            }
            let b: BoundText = serde_json::from_str(text).map_err(|e| Error::runtime(format!("a partition bound is damaged: {e}")))?;
            let value = |t: &str| -> Result<Value> { self.eval_const(&crate::sql::parse_expression(t)?) };
            let edge = |t: &str| -> Result<Edge> {
                Ok(match t.to_ascii_uppercase().as_str() {
                    "MINVALUE" => Edge::Min,
                    "MAXVALUE" => Edge::Max,
                    _ => Edge::At(value(t)?),
                })
            };
            let bound = match b.kind.as_str() {
                "range" => Bound::Range {
                    from: b.from.iter().map(|t| edge(t)).collect::<Result<_>>()?,
                    to: b.to.iter().map(|t| edge(t)).collect::<Result<_>>()?,
                },
                "list" => Bound::List(b.values.iter().map(|t| value(t)).collect::<Result<_>>()?),
                "hash" => Bound::Hash { modulus: b.modulus, remainder: b.remainder },
                _ => Bound::Default,
            };
            out.push((child.to_string(), bound));
        }
        Ok(out)
    }

    /// The table that takes `row` of partitioned `table`, going down through partitioned partitions.
    fn route(&self, table: &str, row: &Row) -> Result<String> {
        let Some(pk) = self.part_key(table) else { return Ok(table.to_string()) };
        let schema = self.table_checked(table)?.schema().clone();
        let keys: Vec<Value> = pk
            .columns
            .iter()
            .map(|c| schema.column_index(c).map(|i| row[i].clone()).ok_or_else(|| Error::catalog(format!("partition key column \"{c}\" is missing"))))
            .collect::<Result<_>>()?;
        let bounds = self.bounds_of(table)?;
        let mut default = None;
        for (child, b) in &bounds {
            let hit = match b {
                Bound::Range { from, to } => {
                    !keys.iter().any(Value::is_null) && tuple_cmp(&keys, from) != Ordering::Less && tuple_cmp(&keys, to) == Ordering::Less
                }
                Bound::List(vs) => vs.iter().any(|v| match (&keys[0], v) {
                    (a, b) if a.is_null() || b.is_null() => a.is_null() && b.is_null(),
                    (a, b) => a.sql_cmp(b) == Some(Ordering::Equal),
                }),
                Bound::Hash { modulus, remainder } => {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    for k in &keys {
                        k.hash(&mut h);
                    }
                    *modulus != 0 && h.finish() % modulus == *remainder
                }
                Bound::Default => {
                    default = Some(child.clone());
                    false
                }
            };
            if hit {
                return self.route(child, row);
            }
        }
        match default {
            Some(d) => self.route(&d, row),
            None => Err(Error::constraint(format!("no partition of relation \"{table}\" found for row"))),
        }
    }

    /// Sends the inserts planned for partitioned `table` to the partitions that take them.
    pub(crate) fn route_partition_inserts(&self, table: &str, ops: Vec<Op>) -> Result<Vec<Op>> {
        let mut next: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut out = Vec::with_capacity(ops.len());
        for op in ops {
            let Op::Insert { row, .. } = op else {
                out.push(op);
                continue;
            };
            let leaf = self.route(table, &row)?;
            let t = self.table_checked(&leaf)?;
            let n = next.entry(key(&leaf)).or_insert(0);
            let id = (t.slot_count() + *n) as crate::storage::RowId;
            *n += 1;
            out.push(Op::Insert { table: t.name().to_string(), id, row });
        }
        Ok(out)
    }
}
