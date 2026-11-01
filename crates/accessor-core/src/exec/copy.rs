//! PostgreSQL's `COPY`: the rows of a table or query written to a file or to the result (`TO`), and a file read into a
//! table (`FROM`). The formats are `text` (tab separated, backslash escapes, `\N` for NULL), `csv` and `json` (a
//! JSON object per line; `TO` only). `COPY .. FROM STDIN` needs the data from the client, which a statement does not
//! carry, so it is refused here.

use super::QueryResult;
use crate::database::Database;
use crate::error::{Error, Result};
use crate::sql::ast::{CopyDef, Expr, From as FromClause, InsertSource, Select, SelectItem, Statement, TableFactor};
use crate::value::Value;

#[derive(Copy, Clone, PartialEq)]
enum Format {
    Text,
    Csv,
    Json,
}

struct Options {
    format: Format,
    delimiter: char,
    null: String,
    header: bool,
    quote: char,
    escape: char,
    force_quote: Vec<String>,
    force_quote_all: bool,
}

fn one_char(name: &str, v: &str) -> Result<char> {
    let mut it = v.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(Error::runtime(format!("COPY {name} must be a single one-byte character"))),
    }
}

fn options_of(def: &CopyDef) -> Result<Options> {
    let get = |n: &str| def.options.iter().rev().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
    let format = match get("format").map(str::to_ascii_lowercase).as_deref() {
        None | Some("text") => Format::Text,
        Some("csv") => Format::Csv,
        Some("json") => Format::Json,
        Some("binary") => return Err(Error::runtime("COPY with the binary format is not available")),
        Some(other) => return Err(Error::runtime(format!("COPY format \"{other}\" not recognized"))),
    };
    let csv = format == Format::Csv;
    let delimiter = match get("delimiter") {
        Some(d) => one_char("delimiter", d)?,
        None if csv => ',',
        None => '\t',
    };
    let quote = get("quote").map(|q| one_char("quote", q)).transpose()?.unwrap_or('"');
    let escape = get("escape").map(|q| one_char("escape", q)).transpose()?.unwrap_or(quote);
    let header = matches!(get("header"), Some(h) if !matches!(h.to_ascii_lowercase().as_str(), "false" | "off" | "0"));
    let force = get("force_quote").unwrap_or("");
    Ok(Options {
        format,
        delimiter,
        null: get("null").map(str::to_string).unwrap_or_else(|| if csv { String::new() } else { "\\N".to_string() }),
        header,
        quote,
        escape,
        force_quote_all: force == "*",
        force_quote: force.split(',').filter(|c| !c.is_empty() && *c != "*").map(|c| c.to_ascii_lowercase()).collect(),
    })
}

fn text_of(v: &Value) -> String {
    match v {
        Value::Bool(b) => (if *b { "t" } else { "f" }).to_string(),
        other => other.to_string(),
    }
}

fn json_of(columns: &[String], row: &[Value]) -> String {
    let mut out = String::from("{");
    for (i, (c, v)) in columns.iter().zip(row).enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::Value::String(c.clone()).to_string());
        out.push(':');
        out.push_str(&match v {
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Float(_) | Value::Double(_) | Value::Dec(_) | Value::Big(_) => v.to_string(),
            Value::Json(s) => s.to_string(),
            other => serde_json::Value::String(text_of(other)).to_string(),
        });
    }
    out.push('}');
    out
}

fn write_text_field(out: &mut String, s: &str, o: &Options) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\u{b}' => out.push_str("\\v"),
            c if c == o.delimiter => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
}

fn write_csv_field(out: &mut String, s: &str, o: &Options, force: bool) {
    let needs = force
        || s.is_empty()
        || s == o.null
        || s.chars().any(|c| c == o.delimiter || c == o.quote || c == '\n' || c == '\r')
        || s.starts_with("\\.");
    if !needs {
        out.push_str(s);
        return;
    }
    out.push(o.quote);
    for c in s.chars() {
        if c == o.quote || c == o.escape {
            out.push(o.escape);
        }
        out.push(c);
    }
    out.push(o.quote);
}

impl Database {
    pub(crate) fn exec_copy(&mut self, def: &CopyDef) -> Result<QueryResult> {
        let o = options_of(def)?;
        if def.to {
            self.copy_to(def, &o)
        } else {
            self.copy_from(def, &o)
        }
    }

    fn copy_to(&mut self, def: &CopyDef, o: &Options) -> Result<QueryResult> {
        let result = match &def.query {
            Some(q) => self.execute_statement(q)?,
            None => {
                self.table_checked(&def.table).map_err(|_| Error::catalog(format!("relation \"{}\" does not exist", def.table)))?;
                let items = if def.columns.is_empty() {
                    vec![SelectItem::Wildcard]
                } else {
                    def.columns.iter().map(|c| SelectItem::Expr { expr: Expr::Column { table: None, name: c.clone() }, alias: None }).collect()
                };
                let select = Select {
                    items,
                    from: Some(FromClause { first: TableFactor::Named { name: def.table.clone(), alias: None, only: false }, joins: Vec::new() }),
                    ..Select::default()
                };
                self.run_select(&select)?
            }
        };
        let mut out = String::new();
        if o.header && o.format != Format::Json {
            let mut line = String::new();
            for (i, c) in result.columns.iter().enumerate() {
                if i > 0 {
                    line.push(o.delimiter);
                }
                if o.format == Format::Csv {
                    write_csv_field(&mut line, c, o, false);
                } else {
                    write_text_field(&mut line, c, o);
                }
            }
            out.push_str(&line);
            out.push('\n');
        }
        let mut lines = 0;
        for row in &result.rows {
            let mut line = String::new();
            if o.format == Format::Json {
                line = json_of(&result.columns, row);
            } else {
                for (i, v) in row.iter().enumerate() {
                    if i > 0 {
                        line.push(o.delimiter);
                    }
                    if v.is_null() {
                        line.push_str(&o.null);
                    } else if o.format == Format::Csv {
                        let name = result.columns.get(i).map(|c| c.to_ascii_lowercase()).unwrap_or_default();
                        write_csv_field(&mut line, &text_of(v), o, o.force_quote_all || o.force_quote.contains(&name));
                    } else {
                        write_text_field(&mut line, &text_of(v), o);
                    }
                }
            }
            out.push_str(&line);
            out.push('\n');
            lines += 1;
        }
        match &def.file {
            Some(path) => {
                std::fs::write(path, out).map_err(|e| Error::runtime(format!("could not open file \"{path}\" for writing: {e}")))?;
                Ok(QueryResult::affected(lines))
            }
            None => Ok(QueryResult {
                columns: vec!["copy".to_string()],
                rows: out.lines().map(|l| vec![Value::text(l)]).collect(),
                affected: lines,
            }),
        }
    }

    fn copy_from(&mut self, def: &CopyDef, o: &Options) -> Result<QueryResult> {
        let Some(path) = &def.file else {
            return Err(Error::runtime("COPY FROM STDIN needs the data to come with the statement; use a file"));
        };
        if o.format == Format::Json {
            return Err(Error::runtime("COPY FROM does not accept the json format"));
        }
        if def.query.is_some() {
            return Err(Error::runtime("COPY (query) FROM is not possible"));
        }
        let bytes = std::fs::read(path).map_err(|e| Error::runtime(format!("could not open file \"{path}\" for reading: {e}")))?;
        let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
        let mut records = if o.format == Format::Csv { parse_csv_records(&text, o) } else { parse_text_records(&text, o) };
        if o.header && !records.is_empty() {
            records.remove(0);
        }
        let width = if def.columns.is_empty() { None } else { Some(def.columns.len()) };
        let mut rows: Vec<Vec<Expr>> = Vec::with_capacity(records.len());
        for r in records {
            if let Some(w) = width {
                if r.len() != w {
                    return Err(Error::runtime(if r.len() < w { "missing data for column" } else { "extra data after last expected column" }));
                }
            }
            rows.push(r.into_iter().map(|f| Expr::Literal(f.map_or(Value::Null, |s| Value::text(&s)))).collect());
        }
        let n = rows.len();
        if n == 0 {
            return Ok(QueryResult::affected(0));
        }
        let columns = if def.columns.is_empty() { None } else { Some(def.columns.clone()) };
        for chunk in rows.chunks(500) {
            self.execute_statement(&Statement::Insert { table: def.table.clone(), columns: columns.clone(), source: InsertSource::Values(chunk.to_vec()) })?;
        }
        Ok(QueryResult::affected(n))
    }
}

/// `None` is NULL.
type Record = Vec<Option<String>>;

fn parse_text_records(text: &str, o: &Options) -> Vec<Record> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line == "\\." {
            break;
        }
        if line.is_empty() {
            continue;
        }
        let mut fields: Record = Vec::new();
        let mut cur = String::new();
        let mut raw_null = true;
        let mut chars = line.chars().peekable();
        let finish = |cur: &mut String, fields: &mut Record, raw_null: &mut bool| {
            fields.push(if *raw_null && *cur == o.null { None } else { Some(std::mem::take(cur)) });
            cur.clear();
            *raw_null = true;
        };
        while let Some(c) = chars.next() {
            if c == o.delimiter {
                finish(&mut cur, &mut fields, &mut raw_null);
            } else if c == '\\' {
                match chars.next() {
                    Some('n') => cur.push('\n'),
                    Some('t') => cur.push('\t'),
                    Some('r') => cur.push('\r'),
                    Some('b') => cur.push('\u{8}'),
                    Some('f') => cur.push('\u{c}'),
                    Some('v') => cur.push('\u{b}'),
                    Some(other) => cur.push(other),
                    None => cur.push('\\'),
                }
                raw_null = false;
            } else {
                cur.push(c);
            }
        }
        finish(&mut cur, &mut fields, &mut raw_null);
        out.push(fields);
    }
    out
}

fn parse_csv_records(text: &str, o: &Options) -> Vec<Record> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Record> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let mut fields: Record = Vec::new();
        loop {
            let mut cur = String::new();
            let mut quoted = false;
            if chars.get(i) == Some(&o.quote) {
                quoted = true;
                i += 1;
                while i < chars.len() {
                    let c = chars[i];
                    if c == o.escape && o.escape != o.quote && i + 1 < chars.len() && (chars[i + 1] == o.quote || chars[i + 1] == o.escape) {
                        cur.push(chars[i + 1]);
                        i += 2;
                    } else if c == o.quote {
                        if chars.get(i + 1) == Some(&o.quote) {
                            cur.push(o.quote);
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    } else {
                        cur.push(c);
                        i += 1;
                    }
                }
            }
            while i < chars.len() && chars[i] != o.delimiter && chars[i] != '\n' {
                cur.push(chars[i]);
                i += 1;
            }
            fields.push(if !quoted && cur == o.null { None } else { Some(cur) });
            if chars.get(i) == Some(&o.delimiter) {
                i += 1;
                continue;
            }
            break;
        }
        if chars.get(i) == Some(&'\n') {
            i += 1;
        }
        if fields.len() == 1 && fields[0].as_deref() == Some("\\.") {
            break;
        }
        out.push(fields);
    }
    out
}
