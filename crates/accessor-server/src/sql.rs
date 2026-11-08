//! Reading SQL text: splitting a script into statements, finding `$n` parameters, command tags and error codes.

use accessor_core::Error;

/// The statements of a script, split at the `;` that end them (not those in strings, quoted names, `$$` bodies or comments).
pub fn split_statements(sql: &str) -> Vec<&str> {
    let b = sql.as_bytes();
    let (mut out, mut start, mut i) = (Vec::new(), 0usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' => {
                let q = b[i];
                i += 1;
                while i < b.len() {
                    if b[i] == q {
                        if b.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 1;
                i += 2;
                while i < b.len() && depth > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'$' => match dollar_tag(&sql[i..]) {
                Some(tag) => {
                    i += tag.len();
                    match sql[i..].find(tag) {
                        Some(end) => i += end + tag.len(),
                        None => i = b.len(),
                    }
                }
                None => i += 1,
            },
            b';' if inside_block(&sql[start..i]) => i += 1,
            b';' => {
                out.push(&sql[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    out.push(&sql[start..]);
    out.into_iter().filter(|s| !is_blank(s)).collect()
}

/// Is `seg` (a statement so far) a `CREATE TRIGGER`, `PROCEDURE` or `FUNCTION` with a `BEGIN ... END` body that is not yet
/// closed? A `;` inside such a body does not end the statement. `BEGIN`/`CASE` open a block, `END` (and `END CASE`) close it;
/// `END IF`, `END LOOP` and `END WHILE` close what `IF`, `LOOP` and `WHILE` opened, which are not counted.
fn inside_block(seg: &str) -> bool {
    let b = seg.as_bytes();
    let (mut i, mut depth, mut words) = (0usize, 0i32, Vec::<String>::new());
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                i += 1;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let from = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                words.push(seg[from..i].to_ascii_uppercase());
            }
            _ => i += 1,
        }
    }
    let head: Vec<&str> = words.iter().take(8).map(String::as_str).collect();
    let routine = head.first() == Some(&"CREATE") && head.iter().any(|w| matches!(*w, "TRIGGER" | "PROCEDURE" | "FUNCTION" | "PROC"));
    if !routine {
        return false;
    }
    for (k, w) in words.iter().enumerate() {
        match w.as_str() {
            "BEGIN" | "CASE" => depth += 1,
            "END" => match words.get(k + 1).map(String::as_str) {
                Some("IF" | "LOOP" | "WHILE" | "REPEAT" | "FOR") => {}
                _ => depth -= 1,
            },
            _ => {}
        }
    }
    depth > 0
}

/// A T-SQL message cut into its batches at the lines that say `GO` (as `sqlcmd` and SSMS do).
pub fn split_batches(text: &str) -> Vec<String> {
    let mut batches = vec![String::new()];
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        let go = t.len() >= 2 && t[..2].eq_ignore_ascii_case("go") && t[2..].trim().chars().all(|c| c.is_ascii_digit());
        if go {
            batches.push(String::new());
        } else {
            batches.last_mut().expect("one batch at least").push_str(line);
        }
    }
    batches.into_iter().filter(|b| !is_blank(b)).collect()
}

/// `$$` or `$tag$` at the start of `s`.
fn dollar_tag(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('$')?;
    let end = rest.find('$')?;
    let name = &rest[..end];
    (name.is_empty() || (!name.starts_with(|c: char| c.is_ascii_digit()) && name.chars().all(|c| c.is_alphanumeric() || c == '_'))).then(|| &s[..end + 2])
}

/// Is there nothing in the text but spaces and comments?
fn is_blank(s: &str) -> bool {
    let t = strip_leading(s);
    t.is_empty()
}

/// The text after leading spaces and comments.
fn strip_leading(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if let Some(r) = s.strip_prefix("--") {
            s = r.find('\n').map_or("", |n| &r[n + 1..]);
        } else if let Some(r) = s.strip_prefix("/*") {
            s = r.find("*/").map_or("", |n| &r[n + 2..]);
        } else {
            return s;
        }
    }
}

/// The largest `$n` in the statement (outside strings and comments).
pub fn parameter_count(sql: &str) -> usize {
    let mut max = 0;
    for_each_parameter(sql, |n| {
        max = max.max(n);
        None
    });
    max
}

/// Replaces each `$n` by `f(n)` (when it gives a text), leaving strings, quoted names, `$$` bodies and comments alone.
pub fn substitute_parameters(sql: &str, mut f: impl FnMut(usize) -> String) -> String {
    for_each_parameter(sql, |n| Some(f(n))).unwrap_or_else(|| sql.to_string())
}

fn for_each_parameter(sql: &str, mut f: impl FnMut(usize) -> Option<String>) -> Option<String> {
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let (mut i, mut copied, mut replaced) = (0usize, 0usize, false);
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' => {
                let q = b[i];
                i += 1;
                while i < b.len() {
                    if b[i] == q {
                        if b.get(i + 1) == Some(&q) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i += 2;
            }
            b'$' if b.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                let start = i;
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                let n: usize = sql[start + 1..i].parse().unwrap_or(0);
                if let Some(text) = f(n) {
                    out.push_str(&sql[copied..start]);
                    out.push_str(&text);
                    copied = i;
                    replaced = true;
                }
            }
            b'$' => match dollar_tag(&sql[i..]) {
                Some(tag) => {
                    i += tag.len();
                    match sql[i..].find(tag) {
                        Some(end) => i += end + tag.len(),
                        None => i = b.len(),
                    }
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    if replaced {
        out.push_str(&sql[copied..]);
        Some(out)
    } else {
        None
    }
}

/// A text as a SQL string literal.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The first few words of a statement in upper case, comments and opening parentheses skipped.
fn words(sql: &str, n: usize) -> Vec<String> {
    let s = strip_leading(sql).trim_start_matches('(');
    s.split_whitespace().take(n).map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_').to_ascii_uppercase()).collect()
}

/// Does the statement return rows (so that its columns can be described before it runs)?
pub fn is_query(sql: &str) -> bool {
    matches!(words(sql, 1).first().map(String::as_str), Some("SELECT" | "WITH" | "VALUES" | "TABLE" | "SHOW" | "EXPLAIN"))
}

/// The tag a `CommandComplete` message carries.
pub fn command_tag(sql: &str, rows: usize, affected: usize, has_columns: bool) -> String {
    let w = words(sql, 6);
    let first = w.first().map(String::as_str).unwrap_or("");
    let count = if has_columns { rows } else { affected };
    match first {
        "SELECT" | "VALUES" | "TABLE" | "WITH" | "EXPLAIN" if has_columns && first != "EXPLAIN" => format!("SELECT {rows}"),
        "EXPLAIN" => "EXPLAIN".into(),
        "INSERT" => format!("INSERT 0 {count}"),
        "UPDATE" | "DELETE" | "MERGE" | "COPY" | "FETCH" | "MOVE" => format!("{first} {count}"),
        "CREATE" | "DROP" | "ALTER" => {
            const MODIFIERS: [&str; 10] = ["OR", "REPLACE", "TEMP", "TEMPORARY", "UNIQUE", "GLOBAL", "LOCAL", "UNLOGGED", "RECURSIVE", "IF"];
            let object = w[1..].iter().find(|x| !MODIFIERS.contains(&x.as_str())).cloned().unwrap_or_default();
            if object == "MATERIALIZED" {
                format!("{first} MATERIALIZED VIEW")
            } else {
                format!("{first} {object}")
            }
        }
        "BEGIN" | "START" => "BEGIN".into(),
        "COMMIT" | "END" => "COMMIT".into(),
        "ROLLBACK" | "ABORT" => "ROLLBACK".into(),
        "" => "".into(),
        other if has_columns => {
            let _ = other;
            format!("SELECT {rows}")
        }
        other => other.to_string(),
    }
}

/// The SQLSTATE for an engine error.
pub fn sqlstate(e: &Error) -> String {
    let m = e.to_string().to_ascii_lowercase();
    let has = |s: &str| m.contains(s);
    match e {
        Error::Syntax(_) => "42601".into(),
        Error::Catalog(_) => {
            if has("already exists") {
                "42P07"
            } else if has("column") && has("does not exist") {
                "42703"
            } else if has("function") && has("does not exist") {
                "42883"
            } else if has("does not exist") || has("unknown table") || has("no such") {
                "42P01"
            } else if has("permission denied") {
                "42501"
            } else {
                "42704"
            }
            .into()
        }
        Error::Constraint(_) => {
            if has("unique") || has("duplicate") {
                "23505"
            } else if has("not-null") || has("not null") || has("null value") {
                "23502"
            } else if has("foreign key") || has("references") || has("referenced") {
                "23503"
            } else if has("check") {
                "23514"
            } else {
                "23000"
            }
            .into()
        }
        Error::Type(_) => {
            if has("out of range") {
                "22003"
            } else {
                "22P02"
            }
            .into()
        }
        Error::Runtime(_) => {
            if has("division by zero") {
                "22012"
            } else if has("permission denied") {
                "42501"
            } else if has("out of range") || has("overflow") {
                "22003"
            } else if has("deadlock") {
                "40P01"
            } else {
                "XX000"
            }
            .into()
        }
        Error::Conflict(_) => "40001".into(),
        Error::Busy(_) => "55P03".into(),
        Error::Locked(_) => "55006".into(),
        Error::Io(_) => "58030".into(),
        Error::Corrupt(_) => "XX001".into(),
        Error::Signal { state, .. } => state.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trigger_body_keeps_its_semicolons() {
        let t = "CREATE TRIGGER t BEFORE INSERT ON a BEGIN RAISE 'x'; RAISE 'y'; END";
        assert_eq!(split_statements(&format!("{t}; select 1")), [t, " select 1"]);
        assert_eq!(split_statements(t), [t]);
        assert_eq!(split_statements("BEGIN; select 1; COMMIT"), ["BEGIN", " select 1", " COMMIT"]);
    }

    #[test]
    fn statements_split_outside_strings_and_bodies() {
        assert_eq!(split_statements("select 1; select 'a;b'; -- c;\n select $$x;y$$ ;"), ["select 1", " select 'a;b'", " -- c;\n select $$x;y$$ "]);
        assert!(split_statements("  -- nothing\n ; /* x */ ").is_empty());
    }

    #[test]
    fn parameters_are_found_and_replaced_outside_strings() {
        assert_eq!(parameter_count("select $1, '$9', $2"), 2);
        assert_eq!(substitute_parameters("select $1, '$9', $2", |n| format!("<{n}>")), "select <1>, '$9', <2>");
    }

    #[test]
    fn tags() {
        assert_eq!(command_tag("insert into t values (1)", 0, 3, false), "INSERT 0 3");
        assert_eq!(command_tag("  select 1", 1, 0, true), "SELECT 1");
        assert_eq!(command_tag("create or replace view v as select 1", 0, 0, false), "CREATE VIEW");
        assert_eq!(command_tag("begin", 0, 0, false), "BEGIN");
    }
}
