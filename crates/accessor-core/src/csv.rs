//! CSV import and export (RFC 4180 quoting).

use crate::decimal::Decimal;
use std::io::{self, Write};

use crate::database::Database;
use crate::datetime;
use crate::error::{Error, Result};
use crate::schema::{Column, TableSchema};
use crate::storage::Op;
use crate::value::{DataType, Row, Value};

fn needs_quotes(s: &str) -> bool {
    s.contains([',', '"', '\n', '\r']) || s.starts_with(' ') || s.ends_with(' ')
}

fn write_field<W: Write>(w: &mut W, s: &str) -> io::Result<()> {
    if needs_quotes(s) {
        write!(w, "\"{}\"", s.replace('"', "\"\""))
    } else {
        w.write_all(s.as_bytes())
    }
}

/// Writes a header line followed by one line per row. `Null` becomes empty.
pub fn write_csv<W: Write>(w: &mut W, columns: &[String], rows: &[Row]) -> io::Result<()> {
    for (i, c) in columns.iter().enumerate() {
        if i > 0 {
            w.write_all(b",")?;
        }
        write_field(w, c)?;
    }
    w.write_all(b"\r\n")?;
    for row in rows {
        for (i, v) in row.iter().enumerate() {
            if i > 0 {
                w.write_all(b",")?;
            }
            write_field(w, &v.to_string())?;
        }
        w.write_all(b"\r\n")?;
    }
    Ok(())
}

/// Splits CSV text into records of fields. Blank lines are skipped.
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut field_started = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => {
                in_quotes = true;
                field_started = true;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                field_started = true;
            }
            '\r' => {}
            '\n' => {
                if field_started || !field.is_empty() || !record.is_empty() {
                    record.push(std::mem::take(&mut field));
                    records.push(std::mem::take(&mut record));
                }
                field_started = false;
            }
            other => {
                field.push(other);
                field_started = true;
            }
        }
    }
    if field_started || !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

fn infer_type(values: &[&str]) -> DataType {
    let non_empty: Vec<&str> = values.iter().copied().filter(|s| !s.trim().is_empty()).collect();
    if non_empty.is_empty() {
        return DataType::Text;
    }
    let all = |f: &dyn Fn(&str) -> bool| non_empty.iter().all(|s| f(s.trim()));
    if all(&|s| s.parse::<i64>().is_ok()) {
        DataType::Integer
    } else if all(&|s| Decimal::parse(s).is_some_and(|d| d.is_integer())) {
        DataType::Bignum
    } else if all(&|s| Decimal::parse(s).is_some()) {
        DataType::Decimal
    } else if all(&|s| matches!(s.to_ascii_lowercase().as_str(), "yes" | "no" | "true" | "false")) {
        DataType::YesNo
    } else if all(&|s| s.contains(['-', '/']) && datetime::parse(s).is_some()) {
        DataType::DateTime
    } else {
        DataType::Text
    }
}

fn convert(field: &str, ty: DataType) -> Result<Value> {
    if field.is_empty() {
        return Ok(Value::Null);
    }
    Value::text(field).coerce_to(ty)
}

/// Appends CSV records to an existing table. With `has_header`, header names
/// are matched to columns; otherwise fields map to columns by position.
/// The import is all-or-nothing. Returns the number of rows added.
pub fn import_csv(db: &mut Database, table: &str, text: &str, has_header: bool) -> Result<usize> {
    let mut records = parse_csv(text);
    let (schema, positions): (TableSchema, Vec<usize>) = {
        let t = db.table_checked(table)?;
        let schema = t.schema().clone();
        let positions = if has_header && !records.is_empty() {
            let header = records.remove(0);
            header
                .iter()
                .map(|h| {
                    schema
                        .column_index(h.trim())
                        .ok_or_else(|| Error::catalog(format!("CSV column '{h}' does not exist in '{}'", schema.name)))
                })
                .collect::<Result<_>>()?
        } else {
            (0..schema.columns.len()).collect()
        };
        (schema, positions)
    };

    let own_tx = !db.in_transaction();
    if own_tx {
        db.begin()?;
    }
    let mut count = 0;
    let result: Result<()> = (|| {
        for (n, rec) in records.iter().enumerate() {
            if rec.len() > positions.len() {
                return Err(Error::runtime(format!("CSV record {} has too many fields", n + 1)));
            }
            let mut vals = Vec::with_capacity(rec.len());
            for (field, &p) in rec.iter().zip(&positions) {
                let v = convert(field, schema.columns[p].data_type)
                    .map_err(|e| Error::type_err(format!("CSV record {}: {e}", n + 1)))?;
                vals.push((p, v));
            }
            db.insert_row(&schema.name, &vals)
                .map_err(|e| Error::runtime(format!("CSV record {}: {e}", n + 1)))?;
            count += 1;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            if own_tx {
                db.commit()?;
            }
            Ok(count)
        }
        Err(e) => {
            if own_tx {
                db.rollback()?;
            }
            Err(e)
        }
    }
}

/// Creates a new table from CSV (first record is the header) with column
/// types inferred from the data, then loads every row.
pub fn import_csv_as_table(db: &mut Database, table: &str, text: &str) -> Result<usize> {
    let records = parse_csv(text);
    let Some((header, body)) = records.split_first() else {
        return Err(Error::runtime("the CSV file is empty"));
    };
    let columns: Vec<Column> = header
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let col: Vec<&str> = body.iter().map(|r| r.get(i).map_or("", String::as_str)).collect();
            let name = if h.trim().is_empty() { format!("Field{}", i + 1) } else { h.trim().to_string() };
            Column::new(name, infer_type(&col))
        })
        .collect();
    let schema = TableSchema { name: table.to_string(), columns, indexes: Vec::new(), foreign_keys: Vec::new() };
    schema.validate()?;
    db.apply_all(vec![Op::CreateTable(schema)])?;
    match import_csv(db, table, text, true) {
        Ok(n) => Ok(n),
        Err(e) => {
            let _ = db.apply_all(vec![Op::DropTable(table.to_string())]);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_quotes_and_newlines() {
        let r = parse_csv("a,b\r\n\"x,1\",\"he said \"\"hi\"\"\"\r\n\r\n\"multi\nline\",\n");
        assert_eq!(r[0], vec!["a", "b"]);
        assert_eq!(r[1], vec!["x,1", "he said \"hi\""]);
        assert_eq!(r[2], vec!["multi\nline", ""]);
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        write_csv(&mut buf, &["n".into(), "t".into()], &[vec![Value::Int(1), "a,\"b\"".into()]]).unwrap();
        let back = parse_csv(std::str::from_utf8(&buf).unwrap());
        assert_eq!(back[1], vec!["1", "a,\"b\""]);
    }

    #[test]
    fn import_infers_types() {
        let mut db = Database::in_memory();
        let n = import_csv_as_table(&mut db, "t", "id,price,when,name\n1,2.5,2020-01-02,ann\n2,3,2021-03-04,\n").unwrap();
        assert_eq!(n, 2);
        let s = db.table("t").unwrap().schema();
        let types: Vec<_> = s.columns.iter().map(|c| c.data_type).collect();
        assert_eq!(types, vec![DataType::Integer, DataType::Decimal, DataType::DateTime, DataType::Text]);
    }

    #[test]
    fn failed_import_rolls_back() {
        let mut db = Database::in_memory();
        db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();
        assert!(import_csv(&mut db, "t", "id\n1\n2\n2\n", true).is_err());
        assert_eq!(db.table("t").unwrap().len(), 0);
    }
}
