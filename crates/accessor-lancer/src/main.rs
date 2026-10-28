//! Finds logic bugs in Accessor the way SQLancer does, without a JDBC driver: random tables and queries, and test oracles
//! that need no second database.
//!
//! * **TLP (ternary logic partitioning)**: for a predicate `p`, the rows of a query are the rows of the same query with
//!   `WHERE p`, `WHERE NOT p` and `WHERE p IS NULL` together; likewise `COUNT`, `SUM`, `MIN`, `MAX` and `DISTINCT`.
//! * **NoREC (non-optimizing reference engine construction)**: `SELECT COUNT(*) ... WHERE p` must equal the number of rows for
//!   which `p` is true when `p` is evaluated in the select list, where the optimizer does not look at it.
//! * **Optimizer comparison**: the same random query, with the optimizer on and off, must give the same rows.
//!
//! A statement that panics is a finding too. Everything is seeded: `--start N` runs a single database again.
//!
//! ```text
//! accessor-lancer [--seeds N] [--start S] [--queries Q] [--verbose]
//! ```

use accessor_core::Database;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len())]
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Ty {
    Int,
    Text,
    Bool,
    Dbl,
}

#[derive(Clone)]
struct Table {
    name: String,
    cols: Vec<(String, Ty)>,
}

/// The columns a query can use: qualified name and type.
type Scope = Vec<(String, Ty)>;

const WORDS: [&str; 8] = ["", "a", "B", "abc", "a%", "xyz", "Abc", "0"];

struct Gen {
    rng: Rng,
    tables: Vec<Table>,
}

impl Gen {
    fn cols_of(&mut self, scope: &Scope, ty: Ty) -> Option<String> {
        let c: Vec<&(String, Ty)> = scope.iter().filter(|(_, t)| *t == ty).collect();
        if c.is_empty() {
            None
        } else {
            Some(c[self.rng.below(c.len())].0.clone())
        }
    }

    fn int(&mut self, scope: &Scope, depth: usize) -> String {
        let leaf = depth == 0 || self.rng.chance(35);
        if leaf {
            return match (self.rng.below(4), self.cols_of(scope, Ty::Int)) {
                (0, _) | (_, None) => {
                    let v = *self.rng.pick(&[0i64, 1, -1, 2, 3, 5, 10, 100, -7, 2147483647]);
                    if v < 0 { format!("({v})") } else { v.to_string() }
                }
                (_, Some(c)) => c,
            };
        }
        match self.rng.below(9) {
            0 => format!("({} + {})", self.int(scope, depth - 1), self.int(scope, depth - 1)),
            1 => format!("({} - {})", self.int(scope, depth - 1), self.int(scope, depth - 1)),
            2 => format!("({} * {})", self.int(scope, depth - 1), self.int(scope, depth - 1)),
            3 => format!("ABS({})", self.int(scope, depth - 1)),
            4 => format!("COALESCE({}, {})", self.int(scope, depth - 1), self.int(scope, depth - 1)),
            5 => format!("(CASE WHEN {} THEN {} ELSE {} END)", self.boolean(scope, depth - 1), self.int(scope, depth - 1), self.int(scope, depth - 1)),
            6 => format!("LENGTH({})", self.text(scope, depth - 1)),
            7 => format!("(-{})", self.int(scope, depth - 1)),
            _ => "NULL".to_string(),
        }
    }

    fn text(&mut self, scope: &Scope, depth: usize) -> String {
        let leaf = depth == 0 || self.rng.chance(40);
        if leaf {
            return match (self.rng.below(3), self.cols_of(scope, Ty::Text)) {
                (0, _) | (_, None) => format!("'{}'", self.rng.pick(&WORDS)),
                (_, Some(c)) => c,
            };
        }
        match self.rng.below(5) {
            0 => format!("UPPER({})", self.text(scope, depth - 1)),
            1 => format!("LOWER({})", self.text(scope, depth - 1)),
            2 => format!("({} || {})", self.text(scope, depth - 1), self.text(scope, depth - 1)),
            3 => format!("COALESCE({}, {})", self.text(scope, depth - 1), self.text(scope, depth - 1)),
            _ => format!("SUBSTR({}, {}, {})", self.text(scope, depth - 1), self.rng.below(3) + 1, self.rng.below(3)),
        }
    }

    /// A predicate an index or a hash join could serve: a column against a constant, or against a column of the same type.
    fn simple(&mut self, scope: &Scope) -> String {
        let (name, ty) = self.rng.pick(scope).clone();
        let op = *self.rng.pick(&["=", "=", "<", ">", "<=", ">=", "<>"]);
        let lit = |g: &mut Gen, ty: Ty| match ty {
            Ty::Int => g.rng.pick(&[0i64, 1, 2, 3, 5, 10, -1]).to_string(),
            Ty::Text => format!("'{}'", g.rng.pick(&WORDS)),
            Ty::Bool => if g.rng.chance(50) { "TRUE" } else { "FALSE" }.to_string(),
            Ty::Dbl => format!("{}.5", g.rng.below(6)),
        };
        match self.rng.below(8) {
            0 => format!("({name} {op} {})", lit(self, ty)),
            1 => format!("({name} = {})", lit(self, ty)),
            2 => format!("({name} IN ({}, {}, {}))", lit(self, ty), lit(self, ty), lit(self, ty)),
            3 => format!("({name} IS NULL)"),
            4 => format!("({name} BETWEEN {} AND {})", lit(self, ty), lit(self, ty)),
            5 if ty == Ty::Text => format!("({name} LIKE '{}%')", self.rng.pick(&["a", "A", "ab", "x", ""])),
            6 => format!("(({name} = {}) OR ({name} = {}))", lit(self, ty), lit(self, ty)),
            _ => match self.cols_of(scope, ty) {
                Some(other) => format!("({name} {op} {other})"),
                None => format!("({name} IS NOT NULL)"),
            },
        }
    }

    /// `EXISTS` / `IN` over another table that refers to the query around it.
    fn correlated(&mut self, scope: &Scope) -> String {
        let t = self.tables[self.rng.below(self.tables.len())].clone();
        let alias = format!("s{}", self.rng.below(100));
        let inner: Scope = t.cols.iter().map(|(c, ty)| (format!("{alias}.{c}"), *ty)).collect();
        let mut both = scope.clone();
        both.extend(inner.clone());
        let ty = *self.rng.pick(&[Ty::Int, Ty::Text]);
        let (outer, mine) = (self.cols_of(scope, ty), self.cols_of(&inner, ty));
        let link = match (outer, mine) {
            (Some(o), Some(m)) => format!("{m} = {o}"),
            _ => self.simple(&both),
        };
        let extra = if self.rng.chance(50) { format!(" AND {}", self.simple(&inner)) } else { String::new() };
        match self.rng.below(3) {
            0 => format!("(EXISTS (SELECT 1 FROM {} AS {alias} WHERE {link}{extra}))", t.name),
            1 => format!("(NOT EXISTS (SELECT 1 FROM {} AS {alias} WHERE {link}{extra}))", t.name),
            _ => match (self.cols_of(&inner, Ty::Int), self.cols_of(scope, Ty::Int)) {
                (Some(m), Some(o)) => format!("({o} IN (SELECT {m} FROM {} AS {alias} WHERE {}))", t.name, self.simple(&inner)),
                _ => format!("(EXISTS (SELECT 1 FROM {} AS {alias} WHERE {link}{extra}))", t.name),
            },
        }
    }

    fn boolean(&mut self, scope: &Scope, depth: usize) -> String {
        if depth > 0 && self.rng.chance(30) {
            return self.simple(scope);
        }
        if depth > 0 && self.rng.chance(10) {
            return self.correlated(scope);
        }
        if depth == 0 || self.rng.chance(15) {
            return match (self.rng.below(5), self.cols_of(scope, Ty::Bool)) {
                (0, Some(c)) => c,
                (1, _) => "TRUE".into(),
                (2, _) => "FALSE".into(),
                (3, _) => "NULL".into(),
                _ => format!("({} {} {})", self.int(scope, 0), self.rng.pick(&["=", "<", ">", "<=", ">=", "<>"]), self.int(scope, 0)),
            };
        }
        let op = *self.rng.pick(&["=", "<", ">", "<=", ">=", "<>"]);
        match self.rng.below(13) {
            0 | 1 => format!("({} {op} {})", self.int(scope, depth - 1), self.int(scope, depth - 1)),
            2 => format!("({} {op} {})", self.text(scope, depth - 1), self.text(scope, depth - 1)),
            3 => format!("({} AND {})", self.boolean(scope, depth - 1), self.boolean(scope, depth - 1)),
            4 => format!("({} OR {})", self.boolean(scope, depth - 1), self.boolean(scope, depth - 1)),
            5 => format!("(NOT {})", self.boolean(scope, depth - 1)),
            6 => format!("({} IS NULL)", self.int(scope, depth - 1)),
            7 => format!("({} IS NOT NULL)", self.text(scope, depth - 1)),
            8 => format!("({} BETWEEN {} AND {})", self.int(scope, depth - 1), self.int(scope, depth - 1), self.int(scope, depth - 1)),
            9 => format!("({} IN ({}, {}, {}))", self.int(scope, depth - 1), self.int(scope, 0), self.int(scope, 0), self.int(scope, 0)),
            10 => format!("({} LIKE '{}')", self.text(scope, depth - 1), self.rng.pick(&["%", "a%", "%b%", "_", "a_c", "%c", ""])),
            11 => {
                // a sub-query on another table
                let t = self.tables[self.rng.below(self.tables.len())].clone();
                let inner: Scope = t.cols.iter().map(|(c, ty)| (format!("{}.{}", t.name, c), *ty)).collect();
                let col = self.cols_of(&inner, Ty::Int);
                match col {
                    Some(c) if self.rng.chance(50) => format!("({} IN (SELECT {c} FROM {}))", self.int(scope, depth - 1), t.name),
                    _ => format!("(EXISTS (SELECT 1 FROM {} WHERE {}))", t.name, self.boolean(&inner, 1)),
                }
            }
            _ => match self.cols_of(scope, Ty::Dbl) {
                Some(c) => format!("({c} {op} {}.5)", self.rng.below(5)),
                None => format!("({} {op} {})", self.int(scope, depth - 1), self.int(scope, depth - 1)),
            },
        }
    }

    /// `FROM` with up to one join: the text and the columns in scope.
    fn from(&mut self) -> (String, Scope) {
        let a = self.tables[self.rng.below(self.tables.len())].clone();
        let scope_a: Scope = a.cols.iter().map(|(c, t)| (format!("{}.{}", a.name, c), *t)).collect();
        if self.tables.len() < 2 || self.rng.chance(55) {
            return (a.name.clone(), scope_a);
        }
        let b = self.tables[self.rng.below(self.tables.len())].clone();
        let real = b.name.clone();
        let alias = if b.name == a.name { format!("{}x", b.name) } else { b.name.clone() };
        let scope_b: Scope = b.cols.iter().map(|(c, t)| (format!("{alias}.{c}"), *t)).collect();
        let mut scope = scope_a;
        scope.extend(scope_b);
        let on = if self.rng.chance(65) {
            let ty = *self.rng.pick(&[Ty::Int, Ty::Text]);
            let ca: Scope = scope.iter().filter(|(n, t)| *t == ty && n.starts_with(&format!("{}.", a.name))).cloned().collect();
            let cb: Scope = scope.iter().filter(|(n, t)| *t == ty && n.starts_with(&format!("{alias}."))).cloned().collect();
            match (self.cols_of(&ca, ty), self.cols_of(&cb, ty)) {
                (Some(x), Some(y)) if self.rng.chance(80) => format!("{x} = {y}"),
                (Some(x), Some(y)) => format!("({x} = {y} OR {x} = 1)"),
                _ => self.boolean(&scope, 2),
            }
        } else {
            self.boolean(&scope, 2)
        };
        let text = match self.rng.below(4) {
            0 => format!("{} CROSS JOIN {} AS {alias}", a.name, real),
            1 => format!("{} JOIN {} AS {alias} ON {on}", a.name, real),
            2 => format!("{} LEFT JOIN {} AS {alias} ON {on}", a.name, real),
            _ => format!("{} RIGHT JOIN {} AS {alias} ON {on}", a.name, real),
        };
        (text, scope)
    }
}

type Rows = Vec<String>;

/// The rows a statement changed.
fn run_count(db: &mut Database, sql: &str) -> Result<usize, String> {
    match catch_unwind(AssertUnwindSafe(|| db.execute(sql))) {
        Err(_) => Err("PANIC".to_string()),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(r)) => Ok(r.affected),
    }
}

/// The column an `UPDATE t SET c = 1000 ...` sets.
fn int_col_name(update: &str) -> String {
    update.split(" SET ").nth(1).and_then(|r| r.split(' ').next()).unwrap_or("c0").to_string()
}

fn run(db: &mut Database, sql: &str) -> Result<Rows, String> {
    match catch_unwind(AssertUnwindSafe(|| db.execute(sql))) {
        Err(_) => Err("PANIC".to_string()),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(r)) => {
            let mut rows: Rows = r.rows.iter().map(|row| row.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("|")).collect();
            rows.sort();
            Ok(rows)
        }
    }
}

struct Finding {
    oracle: &'static str,
    seed: u64,
    detail: String,
    script: String,
}

struct Ctx<'a> {
    db: Database,
    gen: Gen,
    seed: u64,
    script: &'a str,
    findings: Vec<Finding>,
    skipped: usize,
}

impl Ctx<'_> {
    fn report(&mut self, oracle: &'static str, detail: String) {
        self.findings.push(Finding { oracle, seed: self.seed, detail, script: self.script.to_string() });
    }

    fn query(&mut self, optimizer: bool, sql: &str) -> Result<Rows, String> {
        self.db.set_optimizer(optimizer);
        let r = run(&mut self.db, sql);
        self.db.set_optimizer(true);
        r
    }

    /// The predicate over a random `FROM`, and the three partitions.
    fn tlp_where(&mut self) {
        let (from, scope) = self.gen.from();
        let p = self.gen.boolean(&scope, 3);
        let base = format!("SELECT * FROM {from}");
        let parts = format!("{base} WHERE {p} UNION ALL {base} WHERE NOT {p} UNION ALL {base} WHERE ({p}) IS NULL");
        let whole = match self.query(true, &base) {
            Ok(r) => r,
            Err(_) => return self.skipped += 1,
        };
        match self.query(true, &parts) {
            Ok(mut got) => {
                got.sort();
                if got != whole {
                    self.report("TLP WHERE", format!("{parts}\n  gave {} rows, {base} has {}\n  first difference: {}", got.len(), whole.len(), first_difference(&whole, &got)));
                }
            }
            Err(e) if e == "PANIC" => self.report("TLP WHERE", format!("panic in {parts}")),
            Err(e) => self.report("TLP WHERE", format!("{parts}\n  failed ({e}) but {base} works")),
        }
    }

    fn norec(&mut self) {
        let (from, scope) = self.gen.from();
        let p = self.gen.boolean(&scope, 3);
        let optimized = format!("SELECT COUNT(*) FROM {from} WHERE {p}");
        let reference = format!("SELECT CASE WHEN {p} THEN 1 ELSE 0 END FROM {from}");
        let a = self.query(true, &optimized);
        let b = self.query(false, &reference);
        match (a, b) {
            (Ok(a), Ok(b)) => {
                let (x, y) = (a[0].clone(), b.iter().filter(|r| r.as_str() == "1").count().to_string());
                if x != y {
                    self.report("NoREC", format!("{optimized}\n  gave {x}, {reference} gave {y}"));
                }
            }
            (Err(e), _) | (_, Err(e)) if e == "PANIC" => self.report("NoREC", format!("panic in {optimized} or {reference}")),
            (Err(_), Err(_)) => self.skipped += 1,
            (Err(e), Ok(_)) => self.report("NoREC", format!("{optimized}\n  failed ({e}) but the reference works")),
            (Ok(_), Err(e)) => self.report("NoREC", format!("{reference}\n  failed ({e}) but the optimized form works")),
        }
    }

    fn tlp_aggregate(&mut self) {
        let (from, scope) = self.gen.from();
        let Some(col) = self.gen.cols_of(&scope, Ty::Int) else { return };
        let p = self.gen.boolean(&scope, 2);
        let (func, combine) = *self.gen.rng.pick(&[("COUNT", "SUM"), ("MAX", "MAX"), ("MIN", "MIN"), ("SUM", "SUM")]);
        let whole = format!("SELECT {func}({col}) FROM {from}");
        let parts = format!(
            "SELECT {combine}(v) FROM (SELECT {func}({col}) AS v FROM {from} WHERE {p} UNION ALL SELECT {func}({col}) AS v FROM {from} WHERE NOT {p} UNION ALL SELECT {func}({col}) AS v FROM {from} WHERE ({p}) IS NULL) AS q"
        );
        match (self.query(true, &whole), self.query(true, &parts)) {
            (Ok(a), Ok(b)) => {
                // COUNT of nothing is 0 but SUM of the counts of nothing is the same; SUM of nothing is NULL on both sides
                if a != b {
                    self.report("TLP aggregate", format!("{parts}\n  gave {:?}, {whole} gave {:?}", b, a));
                }
            }
            (Err(e), _) | (_, Err(e)) if e == "PANIC" => self.report("TLP aggregate", format!("panic in {parts}")),
            (Ok(_), Err(e)) => self.report("TLP aggregate", format!("{parts}\n  failed ({e}) but {whole} works")),
            _ => self.skipped += 1,
        }
    }

    fn tlp_distinct(&mut self) {
        let (from, scope) = self.gen.from();
        let ty = if self.gen.rng.chance(50) { Ty::Int } else { Ty::Text };
        let Some(col) = self.gen.cols_of(&scope, ty) else { return };
        let p = self.gen.boolean(&scope, 2);
        let whole = format!("SELECT DISTINCT {col} FROM {from}");
        let parts = format!("SELECT {col} FROM {from} WHERE {p} UNION SELECT {col} FROM {from} WHERE NOT {p} UNION SELECT {col} FROM {from} WHERE ({p}) IS NULL");
        match (self.query(true, &whole), self.query(true, &parts)) {
            (Ok(a), Ok(b)) => {
                // text is compared without regard to case, so DISTINCT keeps whichever spelling comes first
                let fold = |r: Rows| -> Rows {
                    let mut v: Rows = r.iter().map(|x| x.to_lowercase()).collect();
                    v.sort();
                    v
                };
                let (a, b) = (fold(a), fold(b));
                if a != b {
                    self.report("TLP DISTINCT", format!("{parts}\n  gave {} rows, {whole} gave {}\n  first difference: {}", b.len(), a.len(), first_difference(&a, &b)));
                }
            }
            (Err(e), _) | (_, Err(e)) if e == "PANIC" => self.report("TLP DISTINCT", format!("panic in {parts}")),
            (Ok(_), Err(e)) => self.report("TLP DISTINCT", format!("{parts}\n  failed ({e}) but {whole} works")),
            _ => self.skipped += 1,
        }
    }

    /// `GROUP BY k HAVING p` against the three partitions of `p`.
    fn tlp_having(&mut self) {
        let (from, scope) = self.gen.from();
        let Some(k) = self.gen.cols_of(&scope, Ty::Int) else { return };
        let c = self.gen.cols_of(&scope, Ty::Int).unwrap_or_else(|| k.clone());
        let lit = self.gen.rng.below(5);
        let p = match self.gen.rng.below(3) {
            0 => format!("COUNT(*) > {lit}"),
            1 => format!("MAX({c}) > {lit}"),
            _ => format!("SUM({c}) < {lit}"),
        };
        let base = format!("SELECT {k}, COUNT(*), MAX({c}) FROM {from} GROUP BY {k}");
        let parts = format!("{base} HAVING {p} UNION ALL {base} HAVING NOT ({p}) UNION ALL {base} HAVING ({p}) IS NULL");
        match (self.query(true, &base), self.query(true, &parts)) {
            (Ok(a), Ok(mut b)) => {
                b.sort();
                if a != b {
                    self.report("TLP HAVING", format!("{parts}\n  gave {} rows, {base} has {}\n  first difference: {}", b.len(), a.len(), first_difference(&a, &b)));
                }
            }
            (Err(e), _) | (_, Err(e)) if e == "PANIC" => self.report("TLP HAVING", format!("panic in {parts}")),
            (Ok(_), Err(e)) => self.report("TLP HAVING", format!("{parts}\n  failed ({e}) but {base} works")),
            _ => self.skipped += 1,
        }
    }

    /// Derived tables, common table expressions and set operations around a filtered query, with the optimizer on and off.
    fn optimizer_difference_nested(&mut self) {
        let (from, scope) = self.gen.from();
        let width = 2 + self.gen.rng.below(2);
        let mut items = Vec::new();
        for i in 0..width {
            let e = match self.gen.rng.below(3) {
                0 => self.gen.int(&scope, 1),
                1 => self.gen.text(&scope, 1),
                _ => self.gen.boolean(&scope, 1),
            };
            items.push(format!("{e} AS v{i}"));
        }
        let inner_filter = self.gen.boolean(&scope, 2);
        let inner = format!("SELECT {} FROM {from} WHERE {inner_filter}", items.join(", "));
        let outer_filter = format!("(d.v0 {} {})", self.gen.rng.pick(&["=", "<", ">", "IS NOT DISTINCT FROM"]), self.gen.rng.below(4));
        let sql = match self.gen.rng.below(5) {
            0 => format!("SELECT * FROM ({inner}) AS d WHERE {outer_filter}"),
            1 => format!("WITH d AS ({inner}) SELECT * FROM d WHERE {outer_filter}"),
            2 => format!("SELECT * FROM ({inner} UNION ALL {inner}) AS d WHERE {outer_filter}"),
            3 => format!("SELECT d.v0, COUNT(*) FROM ({inner}) AS d WHERE {outer_filter} GROUP BY d.v0"),
            _ => format!("SELECT * FROM ({inner}) AS d LEFT JOIN ({inner}) AS e ON d.v0 = e.v0 WHERE {outer_filter}"),
        };
        let on = self.query(true, &sql);
        let off = self.query(false, &sql);
        match (on, off) {
            (Ok(a), Ok(b)) => {
                let (a, b) = (folded(a), folded(b));
                if a != b {
                    self.report("optimizer on/off (nested)", format!("{sql}\n  optimizer on gave {} rows, off gave {}\n  first difference: {}", a.len(), b.len(), first_difference(&b, &a)));
                }
            }
            (Err(e), _) | (_, Err(e)) if e == "PANIC" => self.report("optimizer on/off (nested)", format!("panic in {sql}")),
            (Err(_), Err(_)) => self.skipped += 1,
            (Err(e), Ok(_)) => self.report("optimizer on/off (nested)", format!("{sql}\n  fails with the optimizer ({e}) and works without")),
            (Ok(_), Err(e)) => self.report("optimizer on/off (nested)", format!("{sql}\n  fails without the optimizer ({e}) and works with it")),
        }
    }

    /// `DELETE` and `UPDATE` with a predicate change as many rows as `COUNT(*)` finds, and the rest stay (undone afterwards).
    fn dml(&mut self) {
        let t = self.gen.tables[self.gen.rng.below(self.gen.tables.len())].clone();
        let scope: Scope = t.cols.iter().map(|(c, ty)| (format!("{}.{c}", t.name), *ty)).collect();
        let p = self.gen.boolean(&scope, 3);
        let name = t.name.clone();
        let total = match self.query(true, &format!("SELECT COUNT(*) FROM {name}")) {
            Ok(r) => r[0].parse::<usize>().unwrap_or(0),
            Err(_) => return self.skipped += 1,
        };
        let hit = match self.query(false, &format!("SELECT CASE WHEN {p} THEN 1 ELSE 0 END FROM {name}")) {
            Ok(r) => r.iter().filter(|x| x.as_str() == "1").count(),
            Err(_) => return self.skipped += 1,
        };
        let int_col = t.cols.iter().find(|(_, ty)| *ty == Ty::Int).map(|(c, _)| c.clone());
        let (statement, expect_left) = match (self.gen.rng.chance(50), int_col) {
            (true, _) | (_, None) => (format!("DELETE FROM {name} WHERE {p}"), total - hit),
            (false, Some(c)) => (format!("UPDATE {name} SET {c} = 1000 WHERE {p}"), total),
        };
        let _ = self.db.execute("BEGIN");
        let changed = run_count(&mut self.db, &statement);
        let left = self.db.execute(&format!("SELECT COUNT(*) FROM {name}")).ok().and_then(|r| r.rows.first().map(|x| x[0].to_string()));
        let updated = match &statement[..1] {
            "U" => self.db.execute(&format!("SELECT COUNT(*) FROM {name} WHERE {}", format!("{} = 1000", int_col_name(&statement)))).ok().and_then(|r| r.rows.first().map(|x| x[0].to_string())),
            _ => None,
        };
        let _ = self.db.execute("ROLLBACK");
        match changed {
            Err(e) if e == "PANIC" => self.report("DML", format!("panic in {statement}")),
            Err(_) => self.skipped += 1,
            Ok(n) => {
                if n != hit || left.as_deref() != Some(&expect_left.to_string()) {
                    self.report("DML", format!("{statement}\n  changed {n} rows, the predicate is true for {hit}; {left:?} rows left, expected {expect_left}"));
                }
                if let Some(u) = updated {
                    // rows that already held 1000 count too, so at least the changed rows have it
                    if u.parse::<usize>().unwrap_or(0) < hit {
                        self.report("DML", format!("{statement}\n  only {u} rows hold the new value, expected at least {hit}"));
                    }
                }
            }
        }
    }

    /// Window functions, set operations and sorting with the optimizer on and off.
    fn optimizer_difference_window(&mut self) {
        let (from, scope) = self.gen.from();
        let Some(k) = self.gen.cols_of(&scope, Ty::Int) else { return };
        let o = self.gen.cols_of(&scope, Ty::Int).unwrap_or_else(|| k.clone());
        let p = self.gen.boolean(&scope, 2);
        let sql = match self.gen.rng.below(5) {
            0 => format!("SELECT {k}, {o}, SUM({o}) OVER (PARTITION BY {k}) FROM {from} WHERE {p}"),
            1 => format!("SELECT {k}, COUNT(*) OVER (PARTITION BY {k} ORDER BY {o}), MIN({o}) OVER () FROM {from} WHERE {p}"),
            2 => format!("SELECT {k} FROM {from} WHERE {p} INTERSECT SELECT {k} FROM {from} WHERE NOT {p}"),
            3 => format!("SELECT {k} FROM {from} EXCEPT SELECT {k} FROM {from} WHERE {p}"),
            _ => format!("SELECT {k}, {o} FROM {from} WHERE {p} ORDER BY {k}, {o} LIMIT {} OFFSET {}", 1 + self.gen.rng.below(5), self.gen.rng.below(4)),
        };
        let on = self.query(true, &sql);
        let off = self.query(false, &sql);
        match (on, off) {
            (Ok(a), Ok(b)) => {
                let (a, b) = (folded(a), folded(b));
                if a != b {
                    self.report("optimizer on/off (window, set)", format!("{sql}\n  optimizer on gave {} rows, off gave {}\n  first difference: {}", a.len(), b.len(), first_difference(&b, &a)));
                }
            }
            (Err(e), _) | (_, Err(e)) if e == "PANIC" => self.report("optimizer on/off (window, set)", format!("panic in {sql}")),
            (Err(_), Err(_)) => self.skipped += 1,
            (Err(e), Ok(_)) => self.report("optimizer on/off (window, set)", format!("{sql}\n  fails with the optimizer ({e}) and works without")),
            (Ok(_), Err(e)) => self.report("optimizer on/off (window, set)", format!("{sql}\n  fails without the optimizer ({e}) and works with it")),
        }
    }

    /// A random query with the optimizer on and off.
    fn optimizer_difference(&mut self) {
        let (from, scope) = self.gen.from();
        let n = 1 + self.gen.rng.below(3);
        let mut items = Vec::new();
        for _ in 0..n {
            items.push(match self.gen.rng.below(3) {
                0 => self.gen.int(&scope, 2),
                1 => self.gen.text(&scope, 2),
                _ => self.gen.boolean(&scope, 2),
            });
        }
        let filter = if self.gen.rng.chance(80) { format!(" WHERE {}", self.gen.boolean(&scope, 3)) } else { String::new() };
        let sql = match self.gen.rng.below(4) {
            0 => format!("SELECT {} FROM {from}{filter}", items.join(", ")),
            1 => {
                let k = self.gen.cols_of(&scope, Ty::Int).unwrap_or_else(|| "1".to_string());
                format!("SELECT {k}, COUNT(*), MAX({}) FROM {from}{filter} GROUP BY {k}", items[0])
            }
            2 => {
                let list = items.iter().enumerate().map(|(i, e)| format!("{e} AS x{i}")).collect::<Vec<_>>().join(", ");
                let order = (1..=items.len()).map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
                format!("SELECT {list} FROM {from}{filter} ORDER BY {order} LIMIT {}", 1 + self.gen.rng.below(6))
            }
            _ => format!("SELECT DISTINCT {} FROM {from}{filter}", items.join(", ")),
        };
        let on = self.query(true, &sql);
        let off = self.query(false, &sql);
        match (on, off) {
            (Ok(a), Ok(b)) => {
                let (a, b) = (folded(a), folded(b));
                if a != b {
                    self.report("optimizer on/off", format!("{sql}\n  optimizer on gave {} rows, off gave {}\n  first difference: {}", a.len(), b.len(), first_difference(&b, &a)));
                }
            }
            (Err(e), _) | (_, Err(e)) if e == "PANIC" => self.report("optimizer on/off", format!("panic in {sql}")),
            (Err(_), Err(_)) => self.skipped += 1,
            (Err(e), Ok(_)) => self.report("optimizer on/off", format!("{sql}\n  fails with the optimizer ({e}) and works without")),
            (Ok(_), Err(e)) => self.report("optimizer on/off", format!("{sql}\n  fails without the optimizer ({e}) and works with it")),
        }
    }
}

/// Text is compared without regard to case, so rows that differ only in case are ties: a `LIMIT` or a `DISTINCT` keeps
/// whichever spelling comes first. Compared folded.
fn folded(r: Rows) -> Rows {
    let mut v: Rows = r.iter().map(|x| x.to_lowercase()).collect();
    v.sort();
    v
}

fn first_difference(expected: &Rows, got: &Rows) -> String {
    let mut e = expected.clone();
    let mut g = got.clone();
    e.retain(|r| {
        if let Some(i) = g.iter().position(|x| x == r) {
            g.remove(i);
            false
        } else {
            true
        }
    });
    format!("missing {:?}, extra {:?}", e.first(), g.first())
}

fn build(seed: u64) -> (Database, Gen, String) {
    let mut rng = Rng::new(seed);
    let mut script = String::new();
    let mut db = Database::in_memory();
    let mut tables = Vec::new();
    for t in 0..(1 + rng.below(3)) {
        let name = format!("t{t}");
        let mut cols = Vec::new();
        for c in 0..(2 + rng.below(3)) {
            let ty = *rng.pick(&[Ty::Int, Ty::Int, Ty::Text, Ty::Bool, Ty::Dbl]);
            cols.push((format!("c{c}"), ty));
        }
        let defs: Vec<String> = cols
            .iter()
            .map(|(n, ty)| {
                let ty = match ty {
                    Ty::Int => "INTEGER",
                    Ty::Text => "TEXT",
                    Ty::Bool => "BOOLEAN",
                    Ty::Dbl => "DOUBLE",
                };
                format!("{n} {ty}")
            })
            .collect();
        let _ = writeln!(script, "CREATE TABLE {name} ({});", defs.join(", "));
        for _ in 0..rng.below(14) {
            let vals: Vec<String> = cols
                .iter()
                .map(|(_, ty)| {
                    if rng.chance(20) {
                        return "NULL".to_string();
                    }
                    match ty {
                        Ty::Int => rng.pick(&[0i64, 1, -1, 2, 3, 5, 10, 100, -7]).to_string(),
                        Ty::Text => format!("'{}'", rng.pick(&WORDS)),
                        Ty::Bool => if rng.chance(50) { "TRUE" } else { "FALSE" }.to_string(),
                        Ty::Dbl => format!("{}.5", rng.below(6)),
                    }
                })
                .collect();
            let _ = writeln!(script, "INSERT INTO {name} VALUES ({});", vals.join(", "));
        }
        for (i, (c, _)) in cols.iter().enumerate() {
            if rng.chance(35) {
                let _ = writeln!(script, "CREATE INDEX i_{name}_{i} ON {name} ({c});");
            }
        }
        if cols.len() > 2 && rng.chance(25) {
            let _ = writeln!(script, "CREATE INDEX i_{name}_m ON {name} ({}, {});", cols[0].0, cols[1].0);
        }
        tables.push(Table { name, cols });
    }
    for line in script.lines() {
        if let Err(e) = db.execute(line) {
            let _ = (line, e);
        }
    }
    (db, Gen { rng, tables }, script)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str, default: u64| -> u64 { args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(default) };
    let (seeds, start, queries) = (arg("--seeds", 200), arg("--start", 1), arg("--queries", 60));
    let verbose = args.iter().any(|a| a == "--verbose");
    let mut all: Vec<Finding> = Vec::new();
    let (mut ran, mut skipped) = (0usize, 0usize);
    for seed in start..start + seeds {
        let (db, gen, script) = build(seed);
        let mut ctx = Ctx { db, gen, seed, script: &script, findings: Vec::new(), skipped: 0 };
        for q in 0..queries {
            let step = catch_unwind(AssertUnwindSafe(|| match q % 9 {
                7 => ctx.dml(),
                8 => ctx.optimizer_difference_window(),
                0 => ctx.tlp_where(),
                1 => ctx.norec(),
                2 => ctx.tlp_aggregate(),
                3 => ctx.tlp_distinct(),
                4 => ctx.tlp_having(),
                5 => ctx.optimizer_difference_nested(),
                _ => ctx.optimizer_difference(),
            }));
            if step.is_err() {
                ctx.report("panic", "the generator or the runner panicked".into());
            }
            ran += 1;
        }
        skipped += ctx.skipped;
        all.extend(ctx.findings);
    }
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &all {
        *by.entry(f.oracle).or_default() += 1;
    }
    println!("{ran} checks over {seeds} databases (seeds {start}..{}), {skipped} skipped because the query failed on both sides, {} findings", start + seeds - 1, all.len());
    for (o, n) in &by {
        println!("  {o}: {n}");
    }
    let limit = if verbose { usize::MAX } else { 25 };
    let mut seen = std::collections::HashSet::new();
    for f in all.iter().filter(|f| seen.insert((f.oracle, f.detail.lines().next().unwrap_or("").to_string()))).take(limit) {
        println!("\n=== {} (seed {}) ===\n{}", f.oracle, f.seed, f.detail);
        if verbose {
            println!("--- script ---\n{}", f.script);
        }
    }
}
