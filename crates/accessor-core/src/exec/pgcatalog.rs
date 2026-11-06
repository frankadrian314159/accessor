//! PostgreSQL's system catalogs (`pg_class`, `pg_attribute`, `pg_type`, `pg_proc`, `pg_constraint`, `pg_index`, ...),
//! built from the tables, views, routines, triggers, roles and types the database holds.
//!
//! There are no object identifiers here. The `oid` of a table, a type or a function is its name, which is also what
//! `'name'::regclass` and `::regtype` give, so `oid = 'tbl'::regclass`, a join of `pg_attribute.attrelid` to
//! `pg_class.oid` and `attrelid::regclass` all work, while arithmetic on an `oid` does not.

use super::Row;
use crate::database::Database;
use crate::value::{DataType, Value};

fn t(s: &str) -> Value {
    Value::text(s)
}

fn b(v: bool) -> Value {
    Value::Bool(v)
}

fn i(v: usize) -> Value {
    Value::Int(v as i64)
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// A table name split into its schema and its own name.
fn split(name: &str) -> (&str, &str) {
    name.split_once('.').unwrap_or(("public", name))
}

/// The name PostgreSQL gives a type (`regtype` output).
fn pg_type_name(d: DataType) -> &'static str {
    match d {
        DataType::Integer => "integer",
        DataType::Bignum => "bigint",
        DataType::Float => "real",
        DataType::Double => "double precision",
        DataType::Decimal => "numeric",
        DataType::Currency => "money",
        DataType::Text => "text",
        DataType::YesNo => "boolean",
        DataType::DateTime => "timestamp without time zone",
        DataType::TimestampTz => "timestamp with time zone",
        DataType::Interval => "interval",
        DataType::Json => "jsonb",
        DataType::Array(_) => "anyarray",
        _ => "text",
    }
}

impl Database {
    /// The columns and rows of the catalog `name`, if it is one of PostgreSQL's.
    pub(crate) fn pg_catalog(&self, name: &str) -> Option<(Vec<String>, Vec<Row>)> {
        let mut rows: Vec<Row> = Vec::new();
        let cols = match name.to_ascii_lowercase().as_str() {
            "pg_namespace" => {
                let mut schemas = vec!["pg_catalog".to_string(), "public".to_string(), "information_schema".to_string()];
                schemas.extend(self.catalog.keys().filter(|(k, _)| k == "schema").map(|(_, n)| n.clone()));
                schemas.dedup();
                for s in schemas {
                    rows.push(vec![t(&s), t(&s), t("postgres")]);
                }
                names(&["oid", "nspname", "nspowner"])
            }
            "pg_class" => {
                for tb in self.tables.values() {
                    let (schema, rel) = split(tb.name());
                    let sc = tb.schema();
                    rows.push(vec![
                        t(tb.name()), t(rel), t(schema), t(""), t("postgres"), i(0), t(""), i(0), i(0), Value::Int(tb.len() as i64), i(0), t(""),
                        b(!sc.indexes.is_empty()), b(false), t("p"), t("r"), i(sc.columns.len()), i(0), b(false), b(false), b(false), b(false), b(true), t("d"), b(false), b(self.has_children(tb.name())), Value::Null, Value::Null,
                    ]);
                    for ix in &sc.indexes {
                        rows.push(vec![
                            t(&ix.name), t(&ix.name), t(schema), t(""), t("postgres"), i(0), t(""), i(0), i(0), i(0), i(0), t(""),
                            b(false), b(false), t("p"), t("i"), i(ix.columns.len()), i(0), b(false), b(false), b(false), b(false), b(true), t("n"), b(false), b(false), Value::Null, Value::Null,
                        ]);
                    }
                }
                for (display, _) in self.views.values() {
                    let (schema, rel) = split(display);
                    rows.push(vec![
                        t(display), t(rel), t(schema), t(""), t("postgres"), i(0), t(""), i(0), i(0), i(0), i(0), t(""),
                        b(false), b(false), t("p"), t("v"), i(0), i(0), b(true), b(false), b(false), b(false), b(true), t("n"), b(false), b(false), Value::Null, Value::Null,
                    ]);
                }
                names(&[
                    "oid", "relname", "relnamespace", "reltype", "relowner", "relam", "relfilenode", "reltablespace", "relpages", "reltuples", "relallvisible", "reltoastrelid",
                    "relhasindex", "relisshared", "relpersistence", "relkind", "relnatts", "relchecks", "relhasrules", "relhastriggers", "relrowsecurity", "relforcerowsecurity", "relispopulated", "relreplident", "relispartition", "relhassubclass", "relacl", "reloptions",
                ])
            }
            "pg_attribute" => {
                for tb in self.tables.values() {
                    for (n, c) in tb.schema().columns.iter().enumerate() {
                        rows.push(vec![
                            t(tb.name()), t(&c.name), t(pg_type_name(c.data_type)), Value::Int(-1), i(n + 1), Value::Int(-1), i(0),
                            b(c.not_null || c.primary_key), b(c.default.is_some() || c.autonumber), b(false), t(""), t(""), b(false), b(true), i(0), i(0), Value::Int(-1), t("p"), t(""), Value::Null, Value::Null,
                        ]);
                    }
                }
                names(&[
                    "attrelid", "attname", "atttypid", "attlen", "attnum", "atttypmod", "attndims", "attnotnull", "atthasdef", "atthasmissing", "attidentity", "attgenerated",
                    "attisdropped", "attislocal", "attinhcount", "attcollation", "attstattarget", "attstorage", "attcompression", "attacl", "attoptions",
                ])
            }
            "pg_type" => {
                let builtin = [
                    ("boolean", 1, 'b', 'B'), ("smallint", 2, 'b', 'N'), ("integer", 4, 'b', 'N'), ("bigint", 8, 'b', 'N'), ("real", 4, 'b', 'N'),
                    ("double precision", 8, 'b', 'N'), ("numeric", -1, 'b', 'N'), ("money", 8, 'b', 'N'), ("text", -1, 'b', 'S'), ("character varying", -1, 'b', 'S'),
                    ("character", -1, 'b', 'S'), ("bytea", -1, 'b', 'U'), ("date", 4, 'b', 'D'), ("time without time zone", 8, 'b', 'D'),
                    ("timestamp without time zone", 8, 'b', 'D'), ("timestamp with time zone", 8, 'b', 'D'), ("interval", 16, 'b', 'T'), ("json", -1, 'b', 'U'),
                    ("jsonb", -1, 'b', 'U'), ("uuid", 16, 'b', 'U'), ("oid", 4, 'b', 'N'), ("name", 64, 'b', 'S'), ("anyarray", -1, 'p', 'P'), ("anyelement", -1, 'p', 'P'),
                    ("record", -1, 'p', 'P'), ("void", 4, 'p', 'P'), ("trigger", 4, 'p', 'P'), ("unknown", -2, 'p', 'X'),
                ];
                let mut push = |n: &str, len: i64, ty: char, cat: char, base: &str, not_null: bool| {
                    rows.push(vec![t(n), t(n), t("pg_catalog"), t("postgres"), Value::Int(len), b(len > 0 && len <= 8), t(&ty.to_string()), t(&cat.to_string()), b(false), b(true), t(","), t(""), t(""), t(""), b(not_null), t(base), Value::Int(-1), t("")]);
                };
                for (n, len, ty, cat) in builtin {
                    push(n, len, ty, cat, "", false);
                }
                for (n, ut) in crate::sql::usertypes::snapshot() {
                    let ty = if ut.domain { 'd' } else if !ut.labels.is_empty() { 'e' } else if ut.range.is_some() { 'r' } else if !ut.fields.is_empty() { 'c' } else { 'b' };
                    push(&n, -1, ty, if ty == 'e' { 'E' } else if ty == 'r' { 'R' } else if ty == 'c' { 'C' } else { 'U' }, if ut.domain { &ut.base } else { "" }, ut.not_null);
                }
                names(&[
                    "oid", "typname", "typnamespace", "typowner", "typlen", "typbyval", "typtype", "typcategory", "typispreferred", "typisdefined", "typdelim", "typelem", "typarray", "typinput",
                    "typnotnull", "typbasetype", "typtypmod", "typcollation",
                ])
            }
            "pg_proc" => {
                for r in self.routines.values() {
                    let kind = if r.aggregate.is_some() { "a" } else if r.function { "f" } else { "p" };
                    let ret = if r.trigger { "trigger".to_string() } else { r.returns.map_or("void".to_string(), |d| pg_type_name(d).to_string()) };
                    let args: Vec<String> = r.params.iter().map(|p| if p.poly.is_empty() { p.data_type.map_or("text", pg_type_name).to_string() } else { p.poly.clone() }).collect();
                    let pnames: Vec<Value> = r.params.iter().map(|p| t(&p.name)).collect();
                    rows.push(vec![
                        t(&r.name), t(&r.name), t("public"), t("postgres"), t("sql"), Value::Int(100), Value::Int(0), i(0), t(kind), b(false), b(false), b(false),
                        b(!r.set_columns.is_empty()), t("v"), i(r.params.len()), i(r.params.iter().filter(|p| p.default.is_some()).count()), t(&ret), t(&args.join(" ")),
                        Value::Array(std::sync::Arc::new(pnames)), t(&r.sql),
                    ]);
                }
                names(&[
                    "oid", "proname", "pronamespace", "proowner", "prolang", "procost", "prorows", "provariadic", "prokind", "prosecdef", "proleakproof", "proisstrict", "proretset", "provolatile",
                    "pronargs", "pronargdefaults", "prorettype", "proargtypes", "proargnames", "prosrc",
                ])
            }
            "pg_aggregate" => {
                for r in self.routines.values() {
                    if let Some(a) = &r.aggregate {
                        rows.push(vec![
                            t(&r.name), t("n"), i(0), t(&a.sfunc), a.finalfunc.as_deref().map_or(t(""), t), a.stype.map_or(t("anyelement"), |d| t(pg_type_name(d))), a.initcond.as_deref().map_or(Value::Null, t),
                        ]);
                    }
                }
                names(&["aggfnoid", "aggkind", "aggnumdirectargs", "aggtransfn", "aggfinalfn", "aggtranstype", "agginitval"])
            }
            "pg_constraint" => {
                for tb in self.tables.values() {
                    let sc = tb.schema();
                    let (schema, rel) = split(tb.name());
                    let keys = |cols: &[usize]| Value::Array(std::sync::Arc::new(cols.iter().map(|c| Value::Int(*c as i64 + 1)).collect()));
                    let pk: Vec<usize> = sc.columns.iter().enumerate().filter(|(_, c)| c.primary_key).map(|(n, _)| n).collect();
                    let con = |name: &str, ty: &str, deferrable: bool, deferred: bool, index: &str, referenced: &str, upd: &str, del: &str, key: Value, fkey: Value| {
                        vec![
                            t(name), t(name), t(schema), t(ty), b(deferrable), b(deferred), b(true), t(tb.name()), t(""), t(index), t(referenced), t(upd), t(del),
                            t(if ty == "f" { "s" } else { " " }), b(true), i(0), b(true), key, fkey,
                        ]
                    };
                    if !pk.is_empty() {
                        let n = format!("{rel}_pkey");
                        rows.push(con(&n, "p", false, false, &n, "", " ", " ", keys(&pk), Value::Null));
                    }
                    for ix in sc.indexes.iter().filter(|x| x.unique && x.columns != pk) {
                        rows.push(con(&ix.name, "u", false, false, &ix.name, "", " ", " ", keys(&ix.columns), Value::Null));
                    }
                    for fk in &sc.foreign_keys {
                        let act = |a: &crate::schema::RefAction| match format!("{a:?}").as_str() {
                            "Cascade" => "c",
                            "SetNull" => "n",
                            "SetDefault" => "d",
                            "Restrict" => "r",
                            _ => "a",
                        };
                        rows.push(con(&fk.name, "f", fk.deferrable, fk.initially_deferred, "", &fk.ref_table, act(&fk.on_update), act(&fk.on_delete), keys(&fk.columns), keys(&fk.ref_columns)));
                    }
                }
                names(&[
                    "oid", "conname", "connamespace", "contype", "condeferrable", "condeferred", "convalidated", "conrelid", "contypid", "conindid", "confrelid", "confupdtype", "confdeltype", "confmatchtype",
                    "conislocal", "coninhcount", "connoinherit", "conkey", "confkey",
                ])
            }
            "pg_index" => {
                for tb in self.tables.values() {
                    let sc = tb.schema();
                    let pk: Vec<usize> = sc.columns.iter().enumerate().filter(|(_, c)| c.primary_key).map(|(n, _)| n).collect();
                    for ix in &sc.indexes {
                        let key: Vec<String> = ix.columns.iter().map(|c| (c + 1).to_string()).collect();
                        rows.push(vec![t(&ix.name), t(tb.name()), i(ix.columns.len()), i(ix.columns.len()), b(ix.unique), b(!pk.is_empty() && ix.columns == pk), b(false), b(true), b(false), b(true), b(true), b(true), t(&key.join(" "))]);
                    }
                }
                names(&["indexrelid", "indrelid", "indnatts", "indnkeyatts", "indisunique", "indisprimary", "indisexclusion", "indimmediate", "indisclustered", "indisvalid", "indisready", "indislive", "indkey"])
            }
            "pg_authid" | "pg_roles" => {
                let mut roles: Vec<(String, bool)> = vec![("postgres".to_string(), true)];
                for ((k, n), _) in self.catalog.iter().filter(|((k, _), _)| k == "role") {
                    let _ = k;
                    roles.push((n.clone(), self.is_superuser(n)));
                }
                for (r, sup) in roles {
                    rows.push(vec![t(&r), t(&r), b(sup), b(true), b(sup), b(sup), b(true), b(sup), b(sup), Value::Int(-1), Value::Null, if name.eq_ignore_ascii_case("pg_roles") { t("********") } else { Value::Null }]);
                }
                names(&["oid", "rolname", "rolsuper", "rolinherit", "rolcreaterole", "rolcreatedb", "rolcanlogin", "rolreplication", "rolbypassrls", "rolconnlimit", "rolvaliduntil", "rolpassword"])
            }
            "pg_trigger" => {
                for tr in self.triggers.values() {
                    rows.push(vec![t(&tr.name), t(&tr.table), t(""), t(&tr.name), t(tr.function.as_deref().unwrap_or("")), i(if tr.each_statement { 0 } else { 1 }), t("O"), b(false), t(""), t(""), t(""), b(false), b(false), i(0)]);
                }
                names(&["oid", "tgrelid", "tgparentid", "tgname", "tgfoid", "tgtype", "tgenabled", "tgisinternal", "tgconstrrelid", "tgconstrindid", "tgconstraint", "tgdeferrable", "tginitdeferred", "tgnargs"])
            }
            "pg_database" => {
                rows.push(vec![t("test"), t("test"), t("postgres"), i(6), t("C"), t("C"), b(false), b(true), Value::Int(-1)]);
                names(&["oid", "datname", "datdba", "encoding", "datcollate", "datctype", "datistemplate", "datallowconn", "datconnlimit"])
            }
            "pg_tables" => {
                for tb in self.tables.values() {
                    let (schema, rel) = split(tb.name());
                    rows.push(vec![t(schema), t(rel), t("postgres"), Value::Null, b(!tb.schema().indexes.is_empty()), b(false), b(false), b(false)]);
                }
                names(&["schemaname", "tablename", "tableowner", "tablespace", "hasindexes", "hasrules", "hastriggers", "rowsecurity"])
            }
            "pg_views" => {
                for (display, sql) in self.views.values() {
                    let (schema, rel) = split(display);
                    rows.push(vec![t(schema), t(rel), t("postgres"), t(sql)]);
                }
                names(&["schemaname", "viewname", "viewowner", "definition"])
            }
            "pg_indexes" => {
                for tb in self.tables.values() {
                    let (schema, rel) = split(tb.name());
                    for ix in &tb.schema().indexes {
                        let cols: Vec<&str> = ix.columns.iter().map(|c| tb.schema().columns[*c].name.as_str()).collect();
                        let def = format!("CREATE {}INDEX {} ON {}.{} USING btree ({})", if ix.unique { "UNIQUE " } else { "" }, ix.name, schema, rel, cols.join(", "));
                        rows.push(vec![t(schema), t(rel), t(&ix.name), Value::Null, t(&def)]);
                    }
                }
                names(&["schemaname", "tablename", "indexname", "tablespace", "indexdef"])
            }
            "pg_settings" => {
                for (k, v) in &self.settings {
                    rows.push(vec![t(k), t(v)]);
                }
                names(&["name", "setting"])
            }
            // known to PostgreSQL, but with nothing in them here
            "pg_operator" => names(&["oid", "oprname", "oprnamespace", "oprleft", "oprright", "oprresult", "oprcode"]),
            "pg_depend" => names(&["classid", "objid", "objsubid", "refclassid", "refobjid", "refobjsubid", "deptype"]),
            "pg_locks" => names(&["locktype", "database", "relation", "page", "tuple", "virtualxid", "transactionid", "pid", "mode", "granted"]),
            "pg_description" => names(&["objoid", "classoid", "objsubid", "description"]),
            "pg_inherits" => names(&["inhrelid", "inhparent", "inhseqno", "inhdetachpending"]),
            "pg_am" => names(&["oid", "amname", "amtype"]),
            "pg_extension" => names(&["oid", "extname", "extowner", "extnamespace", "extversion"]),
            "pg_sequence" => names(&["seqrelid", "seqtypid", "seqstart", "seqincrement", "seqmax", "seqmin", "seqcache", "seqcycle"]),
            "pg_foreign_data_wrapper" => {
                for (i, (_, d)) in self.foreign_objects("fdw").into_iter().enumerate() {
                    let s = |k: &str| d.get(k).and_then(|x| x.as_str()).map_or_else(|| t("-"), t);
                    let opts: Vec<Value> = d["options"].as_array().map(|o| o.iter().map(|p| t(&format!("{}={}", p[0].as_str().unwrap_or(""), p[1].as_str().unwrap_or("")))).collect()).unwrap_or_default();
                    rows.push(vec![Value::Int(16384 + i as i64), s("name"), s("owner"), s("handler"), s("validator"), Value::Null, if opts.is_empty() { Value::Null } else { Value::Array(std::sync::Arc::new(opts)) }]);
                }
                names(&["oid", "fdwname", "fdwowner", "fdwhandler", "fdwvalidator", "fdwacl", "fdwoptions"])
            }
            "pg_foreign_server" => {
                for (i, (_, d)) in self.foreign_objects("server").into_iter().enumerate() {
                    let s = |k: &str| d.get(k).and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map_or(Value::Null, t);
                    let opts: Vec<Value> = d["options"].as_array().map(|o| o.iter().map(|p| t(&format!("{}={}", p[0].as_str().unwrap_or(""), p[1].as_str().unwrap_or("")))).collect()).unwrap_or_default();
                    rows.push(vec![Value::Int(16384 + i as i64), s("name"), s("owner"), s("fdw"), s("type"), s("version"), Value::Null, if opts.is_empty() { Value::Null } else { Value::Array(std::sync::Arc::new(opts)) }]);
                }
                names(&["oid", "srvname", "srvowner", "srvfdw", "srvtype", "srvversion", "srvacl", "srvoptions"])
            }
            "pg_user_mapping" => {
                for (i, (_, d)) in self.foreign_objects("umapping").into_iter().enumerate() {
                    let s = |k: &str| d.get(k).and_then(|x| x.as_str()).map_or(Value::Null, t);
                    let opts: Vec<Value> = d["options"].as_array().map(|o| o.iter().map(|p| t(&format!("{}={}", p[0].as_str().unwrap_or(""), p[1].as_str().unwrap_or("")))).collect()).unwrap_or_default();
                    rows.push(vec![Value::Int(16384 + i as i64), s("user"), s("server"), if opts.is_empty() { Value::Null } else { Value::Array(std::sync::Arc::new(opts)) }]);
                }
                names(&["oid", "umuser", "umserver", "umoptions"])
            }
            "pg_policies" => {
                for ((k, n), v) in self.catalog.iter().filter(|((k, _), _)| k == "policy") {
                    let _ = k;
                    let (table, policy) = n.split_once('|').unwrap_or(("", n));
                    let (schema, rel) = split(table);
                    let p: serde_json::Value = serde_json::from_str(v).unwrap_or_default();
                    let text = |key: &str| p.get(key).and_then(|x| x.as_str()).map_or(Value::Null, t);
                    let roles: Vec<Value> = match p.get("roles").and_then(|r| r.as_array()) {
                        Some(r) if !r.is_empty() => r.iter().filter_map(|x| x.as_str()).map(t).collect(),
                        _ => vec![t("public")],
                    };
                    let permissive = p.get("permissive").and_then(|x| x.as_bool()).unwrap_or(true);
                    let cmd = p.get("command").and_then(|x| x.as_str()).unwrap_or("all").to_ascii_uppercase();
                    rows.push(vec![t(schema), t(rel), t(policy), t(if permissive { "PERMISSIVE" } else { "RESTRICTIVE" }), Value::Array(std::sync::Arc::new(roles)), t(&cmd), text("using"), text("check")]);
                }
                names(&["schemaname", "tablename", "policyname", "permissive", "roles", "cmd", "qual", "with_check"])
            }
            "pg_largeobject_metadata" => {
                for (id, _) in crate::expr::pgmore::large_objects() {
                    rows.push(vec![Value::Int(id), t("postgres"), Value::Null]);
                }
                names(&["oid", "lomowner", "lomacl"])
            }
            "pg_largeobject" => {
                for (id, data) in crate::expr::pgmore::large_objects() {
                    for (page, chunk) in data.chunks(2048).enumerate() {
                        rows.push(vec![Value::Int(id), Value::Int(page as i64), Value::Bytes(chunk.to_vec().into())]);
                    }
                }
                names(&["loid", "pageno", "data"])
            }
            "pg_policy" => names(&["oid", "polname", "polrelid", "polcmd", "polpermissive", "polroles", "polqual", "polwithcheck"]),
            "pg_auth_members" => names(&["roleid", "member", "grantor", "admin_option"]),
            "pg_statistic_ext" => names(&["oid", "stxrelid", "stxname", "stxnamespace", "stxowner", "stxkeys", "stxkind"]),
            "pg_attrdef" => names(&["oid", "adrelid", "adnum", "adbin"]),
            "pg_rewrite" => names(&["oid", "rulename", "ev_class", "ev_type", "is_instead"]),
            "pg_collation" => {
                let mut all: Vec<(String, char)> = crate::collation::builtins().into_iter().map(|(n, p)| (n.to_string(), p)).collect();
                all.extend(self.user_collations());
                for (i, (n, p)) in all.into_iter().enumerate() {
                    rows.push(vec![Value::Int(100 + i as i64), t(&n), Value::Int(11), Value::Int(10), t(&p.to_string())]);
                }
                names(&["oid", "collname", "collnamespace", "collowner", "collprovider"])
            }
            "pg_language" => names(&["oid", "lanname", "lanowner", "lanispl"]),
            "pg_cast" => names(&["oid", "castsource", "casttarget", "castfunc", "castcontext", "castmethod"]),
            "pg_tablespace" => names(&["oid", "spcname", "spcowner"]),
            _ => return None,
        };
        Some((cols, rows))
    }
}
