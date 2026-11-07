//! Object identifiers for the PostgreSQL catalogs.
//!
//! Every object the catalogs list (a schema, table, view, index, type, function, constraint, trigger, role) has a number,
//! its `oid`, as in PostgreSQL. Numbers are handed out in the order objects are first seen, from 16384 up (the first user
//! oid of PostgreSQL), and stay the same for the life of the process; the built-in schemas and types have the numbers
//! PostgreSQL gives them (`public` 2200, `integer` 23, `text` 25 ...), which is what clients such as DBeaver map
//! column types by.
//!
//! The registry is shared by all connections, so `oid = 'tbl'::regclass`, `attrelid::regclass` and `pg_get_indexdef(oid)`
//! agree on which number a name has. A name stands for a number in a comparison with an `oid` (`oid = 'tbl'`), and a
//! number stands for its name when it is cast to `regclass`, `regtype` and the like.

use std::collections::HashMap;
use std::sync::Mutex;

/// The kinds of object that have an oid. Two kinds may share a name, so the kind is part of the key.
pub(crate) type Kind = char;
pub(crate) const NAMESPACE: Kind = 'n';
pub(crate) const RELATION: Kind = 'r';
pub(crate) const TYPE: Kind = 't';
pub(crate) const PROC: Kind = 'p';
pub(crate) const CONSTRAINT: Kind = 'c';
pub(crate) const TRIGGER: Kind = 'g';
pub(crate) const ROLE: Kind = 'o';
pub(crate) const DEFAULT: Kind = 'd';
pub(crate) const ACCESS_METHOD: Kind = 'a';

/// The first oid handed out for an object of the user.
const FIRST_USER_OID: i64 = 16384;

/// PostgreSQL's own numbers for its schemas and the types the catalog lists.
const BUILTIN: &[(Kind, &str, i64)] = &[
    (NAMESPACE, "pg_catalog", 11),
    (NAMESPACE, "public", 2200),
    (NAMESPACE, "information_schema", 13182),
    (ROLE, "postgres", 10),
    (ACCESS_METHOD, "heap", 2),
    (ACCESS_METHOD, "btree", 403),
    (TYPE, "boolean", 16),
    (TYPE, "bytea", 17),
    (TYPE, "name", 19),
    (TYPE, "bigint", 20),
    (TYPE, "smallint", 21),
    (TYPE, "integer", 23),
    (TYPE, "text", 25),
    (TYPE, "oid", 26),
    (TYPE, "json", 114),
    (TYPE, "real", 700),
    (TYPE, "double precision", 701),
    (TYPE, "unknown", 705),
    (TYPE, "money", 790),
    (TYPE, "character", 1042),
    (TYPE, "character varying", 1043),
    (TYPE, "date", 1082),
    (TYPE, "time without time zone", 1083),
    (TYPE, "timestamp without time zone", 1114),
    (TYPE, "timestamp with time zone", 1184),
    (TYPE, "interval", 1186),
    (TYPE, "numeric", 1700),
    (TYPE, "record", 2249),
    (TYPE, "anyarray", 2277),
    (TYPE, "void", 2278),
    (TYPE, "trigger", 2279),
    (TYPE, "anyelement", 2283),
    (TYPE, "uuid", 2950),
    (TYPE, "jsonb", 3802),
];

struct Registry {
    by_key: HashMap<(Kind, String), i64>,
    by_oid: HashMap<i64, (Kind, String)>,
    by_name: HashMap<String, Vec<i64>>,
    next: i64,
}

impl Registry {
    fn new() -> Registry {
        let mut r = Registry { by_key: HashMap::new(), by_oid: HashMap::new(), by_name: HashMap::new(), next: FIRST_USER_OID };
        for (kind, name, oid) in BUILTIN {
            r.insert(*kind, name, *oid);
        }
        r
    }

    fn insert(&mut self, kind: Kind, name: &str, oid: i64) {
        self.by_key.insert((kind, name.to_string()), oid);
        self.by_oid.insert(oid, (kind, name.to_string()));
        self.by_name.entry(name.to_ascii_lowercase()).or_default().push(oid);
    }
}

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Registry::new))
}

/// The oid of the object of this kind and name; a number is given the first time a name is asked about.
pub(crate) fn oid(kind: Kind, name: &str) -> i64 {
    with(|r| {
        if let Some(o) = r.by_key.get(&(kind, name.to_string())) {
            return *o;
        }
        let o = r.next;
        r.next += 1;
        r.insert(kind, name, o);
        o
    })
}

/// The oid as a catalog value.
pub(crate) fn value(kind: Kind, name: &str) -> crate::value::Value {
    crate::value::Value::Int(oid(kind, name))
}

/// The name an oid was given for, if it was given.
pub(crate) fn name_of(oid: i64) -> Option<String> {
    with(|r| r.by_oid.get(&oid).map(|(_, n)| n.clone()))
}

/// Whether `name` (in any case, with or without a schema `public.`) is a name the oid `oid` was given for.
pub(crate) fn named(name: &str, oid: i64) -> bool {
    with(|r| {
        let bare = name.strip_prefix("public.").unwrap_or(name).trim_matches('"');
        r.by_name.get(&bare.to_ascii_lowercase()).is_some_and(|list| list.contains(&oid))
    })
}

/// The oid for a name, when only one object has it (a table and a type of the same name are two).
pub(crate) fn find(name: &str) -> Option<i64> {
    with(|r| {
        let bare = name.strip_prefix("public.").unwrap_or(name).trim_matches('"');
        match r.by_name.get(&bare.to_ascii_lowercase()).map(Vec::as_slice) {
            Some([one]) => Some(*one),
            Some(many) if !many.is_empty() => many.iter().copied().find(|o| r.by_oid.get(o).is_some_and(|(k, _)| *k == RELATION)).or(Some(many[0])),
            _ => None,
        }
    })
}

/// The PostgreSQL typmod of a declared type: `varchar(20)` is 24 and `numeric(10,2)` is 655366, `-1` for none.
/// Returns the base type's name, the typmod, and the oid of the base type.
pub(crate) fn type_of_declared(declared: &str) -> (i64, i32) {
    let d = declared.trim();
    if let Some(open) = d.find('(') {
        if let Some(close) = d[open..].find(')') {
            let base = d[..open].trim().to_ascii_lowercase();
            let rest = d[open + close + 1..].trim();
            let args: Vec<i32> = d[open + 1..open + close].split(',').filter_map(|a| a.trim().parse().ok()).collect();
            if rest.is_empty() {
                match (base.as_str(), args.as_slice()) {
                    ("character varying" | "varchar", [n]) => return (oid(TYPE, "character varying"), n + 4),
                    ("character" | "char" | "bpchar", [n]) => return (oid(TYPE, "character"), n + 4),
                    ("numeric" | "decimal", [p]) => return (oid(TYPE, "numeric"), (p << 16) + 4),
                    ("numeric" | "decimal", [p, s]) => return (oid(TYPE, "numeric"), ((p << 16) | s) + 4),
                    _ => {}
                }
            }
        }
        return (oid(TYPE, d), -1);
    }
    (oid(TYPE, d), -1)
}

/// `format_type(oid, typmod)`: the name of a type with its modifier.
pub(crate) fn format_type(type_oid: i64, typmod: i64) -> String {
    let Some(name) = name_of(type_oid) else { return type_oid.to_string() };
    if typmod < 4 {
        return name;
    }
    let m = typmod - 4;
    match name.as_str() {
        "character varying" | "character" => format!("{name}({m})"),
        "numeric" if m >> 16 > 0 => {
            let (p, s) = (m >> 16, m & 0xffff);
            if s == 0 {
                format!("numeric({p})")
            } else {
                format!("numeric({p},{s})")
            }
        }
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_numbers_and_new_ones() {
        assert_eq!(oid(NAMESPACE, "public"), 2200);
        assert_eq!(oid(TYPE, "integer"), 23);
        let a = oid(RELATION, "oid_test_table_a");
        assert!(a >= FIRST_USER_OID);
        assert_eq!(oid(RELATION, "oid_test_table_a"), a);
        assert_eq!(name_of(a).as_deref(), Some("oid_test_table_a"));
        assert!(named("OID_test_table_A", a));
        assert!(named("public.oid_test_table_a", a));
        assert_eq!(find("oid_test_table_a"), Some(a));
    }

    #[test]
    fn declared_types() {
        assert_eq!(type_of_declared("character varying(20)"), (1043, 24));
        assert_eq!(type_of_declared("numeric(10,2)"), (1700, (10 << 16 | 2) + 4));
        assert_eq!(type_of_declared("integer"), (23, -1));
        assert_eq!(format_type(1043, 24), "character varying(20)");
        assert_eq!(format_type(1700, (10 << 16 | 2) + 4), "numeric(10,2)");
        assert_eq!(format_type(1700, -1), "numeric");
    }
}
