//! Schemas, domains, enumerated types, extensions, roles and privileges.
//!
//! These objects have no table of their own. Each is an entry of the database's catalog, `(kind, name)` to a text
//! (`Database::catalog`), changed through [`Op::Catalog`] so that it is logged, saved, and undone by `ROLLBACK`.
//!
//! * **Schemas.** A table of a schema is an ordinary table whose name is `schema.table`; the schema must exist when
//!   the table is created. `public` and `pg_catalog` are the default and are left off the name.
//! * **Domains and enums.** Expanded by the parser: see [`crate::sql::usertypes`].
//! * **Extensions.** Recorded and shown; the functions an extension would bring are built in or absent.
//! * **Roles and privileges.** `SET ROLE r` makes `r` the current role; a role that is not a superuser needs a
//!   `GRANT` of `SELECT`, `INSERT`, `UPDATE` or `DELETE` on a table to use it. Nothing is checked while no role is set,
//!   which is the case until a script runs `SET ROLE`. A table or view made while a role is set is owned by that role,
//!   which has every right on it.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::Ordering;

use super::QueryResult;
use crate::database::{key, Database};
use crate::error::{Error, Result};
use crate::sql::ast::{CatalogChange, GrantDef};
use crate::sql::usertypes::{self, UserType};
use crate::storage::Op;

const TABLE_PRIVILEGES: [&str; 7] = ["select", "insert", "update", "delete", "truncate", "references", "trigger"];

fn describe(kind: &str) -> &str {
    match kind {
        "type" => "type",
        other => other,
    }
}

impl Database {
    /// The query a materialized view was made from.
    pub(crate) fn matview_query(&self, name: &str) -> Option<String> {
        self.catalog.get(&("matview".to_string(), key(name))).cloned()
    }

    pub(crate) fn set_matview(&mut self, name: &str, query: Option<String>) -> Result<()> {
        self.catalog_set("matview", name, query)
    }

    /// The invisible columns of a table (MySQL's `INVISIBLE`), by lower-case name.
    pub(crate) fn invisible_columns(&self, table: &str) -> Vec<String> {
        let prefix = format!("{}.", key(table));
        self.catalog
            .keys()
            .filter(|(k, _)| k == "invisible")
            .filter_map(|(_, n)| n.strip_prefix(&prefix))
            .filter(|c| !c.contains('.'))
            .map(String::from)
            .collect()
    }

    /// Makes a column of a table invisible or visible again.
    pub(crate) fn set_invisible(&mut self, table: &str, column: &str, invisible: bool) -> Result<()> {
        let name = format!("{}.{}", key(table), column.to_ascii_lowercase());
        let present = self.catalog_has("invisible", &name);
        if invisible != present {
            self.catalog_set("invisible", &name, invisible.then(|| "1".to_string()))?;
        }
        Ok(())
    }

    /// Forgets the invisible columns of a table that is dropped.
    pub(crate) fn forget_invisible(&mut self, table: &str) -> Result<()> {
        for c in self.invisible_columns(table) {
            self.set_invisible(table, &c, false)?;
        }
        Ok(())
    }

    /// Records that a column is of a range or composite type, so that a database opened again still puts values in its form.
    pub(crate) fn set_column_type(&mut self, table: &str, column: &str, type_name: &str) -> Result<()> {
        let name = format!("{}.{}", key(table), column.to_ascii_lowercase());
        self.catalog_set("coltype", &name, Some(type_name.to_string()))
    }

    /// Forgets the column types of a table that is dropped.
    pub(crate) fn forget_column_types(&mut self, table: &str) -> Result<()> {
        let prefix = format!("{}.", key(table));
        let names: Vec<String> = self.catalog.keys().filter(|(k, n)| k == "coltype" && n.starts_with(&prefix)).map(|(_, n)| n.clone()).collect();
        for n in names {
            self.catalog_set("coltype", &n, None)?;
        }
        Ok(())
    }

    /// Puts the range and composite column types of the catalog in the registry of this session.
    pub(crate) fn register_column_types(&self) {
        for ((k, n), v) in &self.catalog {
            if k == "coltype" {
                if let Some((table, column)) = n.rsplit_once('.') {
                    crate::expr::ranges::register_column(table, column, v);
                }
            }
        }
    }

    fn catalog_has(&self, kind: &str, name: &str) -> bool {
        self.catalog.contains_key(&(kind.to_string(), key(name)))
    }

    fn catalog_set(&mut self, kind: &str, name: &str, value: Option<String>) -> Result<()> {
        self.apply_all(vec![Op::Catalog { kind: kind.to_string(), name: name.to_string(), value }])
    }

    /// The domains and enums, for the parser.
    pub(crate) fn user_types(&self) -> HashMap<String, UserType> {
        self.catalog
            .iter()
            .filter(|((k, _), _)| k == "type")
            .filter_map(|((_, n), v)| serde_json::from_str::<UserType>(v).ok().map(|t| (n.clone(), t)))
            .collect()
    }

    /// Makes the collations made with `CREATE COLLATION` the ones the sort keys use.
    pub(crate) fn sync_collations(&self) {
        let defined: Vec<(String, String)> = self.catalog.iter().filter(|((k, _), _)| k == "collation").map(|((_, n), v)| (n.clone(), v.clone())).collect();
        if !defined.is_empty() || crate::collation::any_defined() {
            crate::collation::sync(&defined);
        }
    }

    /// The collations made with `CREATE COLLATION`: (name, provider letter).
    pub(crate) fn user_collations(&self) -> Vec<(String, char)> {
        self.catalog
            .iter()
            .filter(|((k, _), _)| k == "collation")
            .map(|((_, n), v)| (n.clone(), if serde_json::from_str::<serde_json::Value>(v).ok().and_then(|j| j.get("provider").and_then(|p| p.as_str().map(String::from))).as_deref() == Some("libc") { 'c' } else { 'i' }))
            .collect()
    }

    /// `CREATE` / `DROP` of a schema, type, extension or role.
    pub(crate) fn exec_catalog(&mut self, c: &CatalogChange) -> Result<QueryResult> {
        if c.kind == "collation" {
            for n in &c.names {
                match &c.value {
                    Some(json) => {
                        let exists = self.catalog_has("collation", n) || crate::collation::builtins().iter().any(|(b, _)| b.eq_ignore_ascii_case(n));
                        if exists {
                            if c.replace {
                                // IF NOT EXISTS
                                continue;
                            }
                            return Err(Error::catalog(format!("collation \"{n}\" for encoding \"UTF8\" already exists")));
                        }
                        self.catalog_set("collation", n, Some(json.clone()))?;
                    }
                    None => {
                        if !self.catalog_has("collation", n) {
                            if c.replace {
                                continue;
                            }
                            return Err(Error::catalog(format!("collation \"{n}\" for encoding \"UTF8\" does not exist")));
                        }
                        self.catalog_set("collation", n, None)?;
                    }
                }
            }
            self.sync_collations();
            return Ok(QueryResult::default());
        }
        if c.kind == "foreign" {
            return self.exec_foreign(c);
        }
        if c.kind == "operator" {
            // an operator is known here by its spelling and shape, not its operand types: a second definition replaces the
            // first and dropping one that is not there is not an error
            for n in &c.names {
                self.catalog_set("operator", n, c.value.clone())?;
            }
            return Ok(QueryResult::default());
        }
        if c.kind == "policy_alter" {
            return self.alter_policy(&c.names[0], c.value.as_deref().unwrap_or("{}"));
        }
        for name in &c.names {
            let what = describe(c.kind);
            match &c.value {
                Some(value) => {
                    if self.catalog_has(c.kind, name) && !c.replace {
                        return Err(Error::catalog(format!("{what} \"{name}\" already exists")));
                    }
                    if c.kind == "type" && !c.replace && self.catalog_has("type", name) {
                        return Err(Error::catalog(format!("type \"{name}\" already exists")));
                    }
                    self.catalog_set(c.kind, name, Some(value.clone()))?;
                }
                None => {
                    if !self.catalog_has(c.kind, name) {
                        return Err(Error::catalog(format!("{what} \"{name}\" does not exist")));
                    }
                    self.drop_catalog_object(c.kind, name, c.cascade)?;
                }
            }
        }
        Ok(QueryResult::default())
    }

    /// A table that is dropped is no longer a foreign table.
    pub(crate) fn forget_foreign_table(&mut self, name: &str) -> Result<()> {
        if self.catalog_has("ftable", name) {
            self.catalog_set("ftable", name, None)?;
        }
        Ok(())
    }

    /// The foreign-data objects of a kind, as `(name, details)` in name order.
    pub(crate) fn foreign_objects(&self, kind: &str) -> Vec<(String, serde_json::Value)> {
        self.catalog
            .iter()
            .filter(|((k, _), _)| k == kind)
            .map(|((_, n), v)| (n.clone(), serde_json::from_str(v).unwrap_or_default()))
            .collect()
    }

    /// CREATE / ALTER / DROP of a foreign-data wrapper, a foreign server or a user mapping.
    fn exec_foreign(&mut self, c: &CatalogChange) -> Result<QueryResult> {
        use serde_json::{json, Value as J};
        let [verb, object, name, server] = &c.names[..] else { return Err(Error::runtime("malformed foreign-data statement")) };
        let a: J = serde_json::from_str(c.value.as_deref().unwrap_or("{}")).unwrap_or_default();
        let flag = |k: &str| a.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        let text = |k: &str| a.get(k).and_then(|v| v.as_str()).map(str::to_string);
        let role = self.settings.get("role").filter(|r| !r.is_empty() && !r.eq_ignore_ascii_case("none")).cloned().unwrap_or_else(|| "postgres".to_string());
        let (kind, id, what) = match object.as_str() {
            "fdw" => ("fdw", key(name), format!("foreign-data wrapper \"{name}\"")),
            "server" => ("server", key(name), format!("server \"{name}\"")),
            "ftable" => ("ftable", key(name), format!("foreign table \"{name}\"")),
            _ => {
                let user = if name.eq_ignore_ascii_case("current_user") || name.eq_ignore_ascii_case("user") { role.clone() } else { name.clone() };
                ("umapping", format!("{}|{}", key(&user), key(server)), format!("user mapping for \"{user}\" for server \"{server}\""))
            }
        };
        let existing: Option<J> = self.catalog.get(&(kind.to_string(), id.clone())).and_then(|v| serde_json::from_str(v).ok());
        let missing = if object == "mapping" {
            format!("user mapping for \"{}\" does not exist for server \"{server}\"", id.split('|').next().unwrap_or(""))
        } else {
            format!("{what} does not exist")
        };
        // who may do what: a role that is not a superuser needs to own the object, or to have been granted USAGE
        let acting = self.settings.get("role").filter(|r| !r.is_empty() && !r.eq_ignore_ascii_case("none")).cloned();
        let restricted = acting.as_ref().is_some_and(|r| !self.effective_roles(r).iter().any(|x| self.is_superuser(x)));
        let owns = |d: &J| -> bool {
            !restricted || acting.as_ref().is_some_and(|r| d["owner"].as_str().is_some_and(|o| self.effective_roles(r).contains(&key(o))))
        };
        let usage = |k: &str, obj: &str| -> bool {
            !restricted
                || acting.as_ref().is_some_and(|r| {
                    self.effective_roles(r).iter().any(|x| {
                        let p = self.privileges_of(x, k, obj);
                        p.contains("usage") || p.contains("all")
                    })
                })
        };
        let lookup = |k: &str, n: &str| -> Option<J> { self.catalog.get(&(k.to_string(), key(n))).and_then(|v| serde_json::from_str(v).ok()) };
        // the object whose owner decides: a mapping answers to its server
        let (guard_kind, guard_name, guard_word) = match object.as_str() {
            "fdw" => ("fdw", name.clone(), "foreign-data wrapper"),
            "server" => ("server", name.clone(), "foreign server"),
            "mapping" | "ftable" => ("server", server.clone(), "foreign server"),
            _ => ("", String::new(), ""),
        };
        let old_options = |d: &J| -> Vec<(String, String)> {
            d.get("options")
                .and_then(|o| o.as_array())
                .map(|o| o.iter().map(|p| (p[0].as_str().unwrap_or("").to_string(), p[1].as_str().unwrap_or("").to_string())).collect())
                .unwrap_or_default()
        };
        let apply = |mut cur: Vec<(String, String)>| -> Result<Vec<(String, String)>> {
            for op in a.get("options").and_then(|o| o.as_array()).cloned().unwrap_or_default() {
                let (act, k, v) = (op[0].as_str().unwrap_or(""), op[1].as_str().unwrap_or("").to_string(), op[2].as_str().unwrap_or("").to_string());
                let pos = cur.iter().position(|(n, _)| *n == k);
                match (act, pos) {
                    ("drop", Some(p)) => {
                        cur.remove(p);
                    }
                    ("set", Some(p)) => cur[p].1 = v,
                    ("drop" | "set", None) => return Err(Error::catalog(format!("option \"{k}\" not found"))),
                    (_, Some(_)) => return Err(Error::catalog(format!("option \"{k}\" provided more than once"))),
                    (_, None) => cur.push((k, v)),
                }
            }
            Ok(cur)
        };
        let options_json = |o: Vec<(String, String)>| -> J { J::Array(o.into_iter().map(|(k, v)| json!([k, v])).collect()) };
        let save = |db: &mut Self, kind: &str, id: &str, d: J| db.catalog_set(kind, id, Some(d.to_string()));
        if verb == "ALTER" || verb == "DROP" {
            if let Some(d) = &existing {
                let guard = if object == "mapping" || object == "ftable" { lookup(guard_kind, &guard_name) } else { Some(d.clone()) };
                if let Some(g) = guard {
                    let own_mapping = object == "mapping" && id.split('|').next() == acting.as_ref().map(|r| key(r)).as_deref();
                    if !owns(&g) && !(own_mapping && usage("server", &guard_name)) && object != "ftable" {
                        return Err(Error::catalog(format!("must be owner of {guard_word} {guard_name}")));
                    }
                    if verb == "ALTER" && object == "fdw" && restricted && (text("handler").is_some() || text("validator").is_some()) {
                        return Err(Error::catalog(format!("permission denied to alter foreign-data wrapper \"{name}\"")));
                    }
                    if verb == "ALTER" && object == "fdw" && restricted && text("owner").is_some() {
                        return Err(Error::catalog(format!("permission denied to change owner of foreign-data wrapper \"{name}\"")));
                    }
                }
            }
        }
        if verb == "CREATE" {
            match object.as_str() {
                "fdw" if restricted => return Err(Error::catalog(format!("permission denied to create foreign-data wrapper \"{name}\""))),
                "server" => {
                    let f = text("fdw").unwrap_or_default();
                    if let Some(p) = lookup("fdw", &f) {
                        if !owns(&p) && !usage("fdw", &f) {
                            return Err(Error::catalog(format!("permission denied for foreign-data wrapper {f}")));
                        }
                    }
                }
                "mapping" | "ftable" => {
                    if let Some(p) = lookup("server", server) {
                        let own = object == "mapping" && key(name) == "current_user" || acting.as_ref().is_some_and(|r| key(r) == key(name));
                        let allowed = if object == "ftable" || own { owns(&p) || usage("server", server) } else { owns(&p) };
                        if !allowed {
                            return Err(Error::catalog(if object == "ftable" || own { format!("permission denied for foreign server {server}") } else { format!("must be owner of foreign server {server}") }));
                        }
                    }
                }
                _ => {}
            }
        }
        match verb.as_str() {
            "CREATE" => {
                if existing.is_some() {
                    if flag("if_not_exists") {
                        return Ok(QueryResult::default());
                    }
                    let msg = if object == "mapping" { format!("user mapping for \"{}\" already exists for server \"{server}\"", id.split('|').next().unwrap_or("")) } else { format!("{what} already exists") };
                    return Err(Error::catalog(msg));
                }
                let mut d = json!({"name": name, "owner": role});
                if object != "fdw" {
                    let parent = if object == "server" { text("fdw").unwrap_or_default() } else { server.clone() };
                    let parent_kind = if object == "server" { "fdw" } else { "server" };
                    let Some(p) = self.catalog.get(&(parent_kind.to_string(), key(&parent))).and_then(|v| serde_json::from_str::<J>(v).ok()) else {
                        return Err(Error::catalog(if object == "server" { format!("foreign-data wrapper \"{parent}\" does not exist") } else { format!("server \"{server}\" does not exist") }));
                    };
                    d["fdw"] = if object == "server" { json!(p["name"]) } else { json!(parent) };
                    if object == "ftable" {
                        d["server"] = json!(p["name"]);
                    }
                    if object == "mapping" {
                        d["user"] = json!(id.split('|').next().unwrap_or(""));
                        d["server"] = json!(p["name"]);
                    }
                }
                for k in ["handler", "validator", "type", "version"] {
                    if let Some(v) = text(k) {
                        d[k] = json!(v);
                    }
                }
                d["options"] = options_json(apply(Vec::new())?);
                save(self, kind, &id, d)?;
            }
            "ALTER" => {
                let Some(mut d) = existing else { return Err(Error::catalog(missing)) };
                d["options"] = options_json(apply(old_options(&d))?);
                for k in ["handler", "validator", "type", "version", "owner"] {
                    if let Some(v) = text(k) {
                        d[k] = json!(v);
                    }
                }
                if let Some(new) = text("rename") {
                    self.catalog_set(kind, &id, None)?;
                    d["name"] = json!(new);
                    if object == "fdw" {
                        for (n, mut s) in self.foreign_objects("server") {
                            if s["fdw"].as_str().is_some_and(|f| key(f) == id) {
                                s["fdw"] = json!(new);
                                save(self, "server", &n, s)?;
                            }
                        }
                    }
                    save(self, kind, &key(&new), d)?;
                } else {
                    save(self, kind, &id, d)?;
                }
            }
            _ => {
                if existing.is_none() {
                    if flag("if_exists") {
                        return Ok(QueryResult::default());
                    }
                    return Err(Error::catalog(missing));
                }
                let mut doomed: Vec<(&str, String)> = Vec::new();
                match object.as_str() {
                    "fdw" => {
                        for (n, s) in self.foreign_objects("server") {
                            if s["fdw"].as_str().is_some_and(|f| key(f) == id) {
                                doomed.push(("server", n.clone()));
                                for (m, u) in self.foreign_objects("umapping") {
                                    if u["server"].as_str().is_some_and(|f| key(f) == n) {
                                        doomed.push(("umapping", m));
                                    }
                                }
                            }
                        }
                    }
                    "server" => {
                        for (m, u) in self.foreign_objects("umapping") {
                            if u["server"].as_str().is_some_and(|f| key(f) == id) {
                                doomed.push(("umapping", m));
                            }
                        }
                        for (m, u) in self.foreign_objects("ftable") {
                            if u["server"].as_str().is_some_and(|f| key(f) == id) {
                                doomed.push(("ftable", m));
                            }
                        }
                    }
                    _ => {}
                }
                if !doomed.is_empty() && !c.cascade {
                    return Err(Error::catalog(format!("cannot drop {} {} because other objects depend on it", if object == "fdw" { "foreign-data wrapper" } else { "server" }, name)));
                }
                for (k, n) in doomed {
                    if k == "ftable" {
                        let _ = self.exec_drop_table(&n);
                    }
                    self.catalog_set(k, &n, None)?;
                }
                self.catalog_set(kind, &id, None)?;
            }
        }
        Ok(QueryResult::default())
    }

    /// `ALTER POLICY`: the parts of the change replace those of the policy; `rename` gives it a new name.
    fn alter_policy(&mut self, name: &str, change: &str) -> Result<QueryResult> {
        let Some(old) = self.catalog.get(&("policy".to_string(), key(name))).cloned() else {
            let (table, policy) = name.split_once('|').unwrap_or(("", name));
            return Err(Error::catalog(format!("policy \"{policy}\" for table \"{table}\" does not exist")));
        };
        let mut policy: serde_json::Value = serde_json::from_str(&old).map_err(|e| Error::runtime(e.to_string()))?;
        let change: serde_json::Value = serde_json::from_str(change).map_err(|e| Error::runtime(e.to_string()))?;
        let mut new_name = None;
        if let (Some(p), Some(c)) = (policy.as_object_mut(), change.as_object()) {
            for (k, v) in c {
                if k == "rename" {
                    new_name = v.as_str().map(str::to_string);
                } else {
                    p.insert(k.clone(), v.clone());
                }
            }
        }
        let text = policy.to_string();
        match new_name {
            Some(n) => {
                let table = name.split_once('|').map_or("", |(t, _)| t);
                let renamed = format!("{table}|{n}");
                if self.catalog_has("policy", &renamed) {
                    return Err(Error::catalog(format!("policy \"{n}\" for table \"{table}\" already exists")));
                }
                self.catalog_set("policy", name, None)?;
                self.catalog_set("policy", &renamed, Some(text))?;
            }
            None => self.catalog_set("policy", name, Some(text))?,
        }
        Ok(QueryResult::default())
    }

    fn drop_catalog_object(&mut self, kind: &str, name: &str, cascade: bool) -> Result<()> {
        match kind {
            "schema" => {
                let prefix = format!("{}.", key(name));
                let tables: Vec<String> =
                    self.table_names().into_iter().filter(|t| key(t).starts_with(&prefix)).map(String::from).collect();
                let views: Vec<String> =
                    self.view_names().into_iter().filter(|t| key(t).starts_with(&prefix)).map(String::from).collect();
                // MySQL's DROP DATABASE drops what is in it
                let cascade = cascade || crate::dialect::current() == crate::dialect::Dialect::Mysql;
                if (!tables.is_empty() || !views.is_empty()) && !cascade {
                    return Err(Error::catalog(format!(
                        "cannot drop schema {name} because other objects depend on it (use DROP SCHEMA {name} CASCADE)"
                    )));
                }
                for v in views {
                    self.exec_drop_view(&v)?;
                }
                // children before parents is not known here, so drop the ones nothing refers to first and repeat
                let mut left = tables;
                while !left.is_empty() {
                    let before = left.len();
                    let mut next = Vec::new();
                    for t in left {
                        if self.exec_drop_table(&t).is_err() {
                            next.push(t);
                        }
                    }
                    if next.len() == before {
                        return Err(Error::catalog(format!("cannot drop schema {name}: its tables refer to each other from outside")));
                    }
                    left = next;
                }
            }
            "role" => {
                let low = key(name);
                let doomed: Vec<String> = self
                    .catalog
                    .keys()
                    .filter(|(k, n)| {
                        (k == "grant" && n.split('|').next() == Some(low.as_str()))
                            || (k == "member" && n.split('|').any(|p| p == low))
                    })
                    .map(|(k, n)| format!("{k}\u{1}{n}"))
                    .collect();
                for d in doomed {
                    let (k, n) = d.split_once('\u{1}').expect("separator");
                    self.catalog_set(k, n, None)?;
                }
                if self.settings.get("role").map(|r| key(r)) == Some(low) {
                    self.settings.remove("role");
                }
            }
            _ => {}
        }
        self.catalog_set(kind, name, None)?;
        if kind == "type" {
            usertypes::unregister(name);
        }
        Ok(())
    }

    /// Checks that a table's schema exists.
    pub(crate) fn check_schema_exists(&self, table: &str) -> Result<()> {
        if let Some((schema, _)) = table.split_once('.') {
            if !schema.eq_ignore_ascii_case("information_schema") && !self.catalog_has("schema", schema) {
                return Err(Error::catalog(format!("schema \"{schema}\" does not exist")));
            }
        }
        Ok(())
    }

    // ---- roles ----

    fn role_options_of(&self, name: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
        let text = self.catalog.get(&("role".to_string(), key(name)))?;
        match serde_json::from_str::<serde_json::Value>(text) {
            Ok(serde_json::Value::Object(m)) => Some(m),
            _ => Some(serde_json::Map::new()),
        }
    }

    pub(crate) fn is_superuser(&self, role: &str) -> bool {
        self.role_options_of(role)
            .and_then(|m| m.get("superuser").and_then(|v| v.as_str().map(|s| s == "true")))
            .unwrap_or(false)
    }

    /// `ALTER ROLE`.
    pub(crate) fn exec_alter_role(&mut self, name: &str, rename: Option<&str>, options: &[(String, String)]) -> Result<QueryResult> {
        let Some(mut map) = self.role_options_of(name) else {
            return Err(Error::catalog(format!("role \"{name}\" does not exist")));
        };
        if let Some(new) = rename {
            if self.catalog_has("role", new) {
                return Err(Error::catalog(format!("role \"{new}\" already exists")));
            }
            let text = serde_json::Value::Object(map).to_string();
            self.catalog_set("role", new, Some(text))?;
            self.catalog_set("role", name, None)?;
            return Ok(QueryResult::default());
        }
        for (k, v) in options {
            map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        self.catalog_set("role", name, Some(serde_json::Value::Object(map).to_string()))?;
        Ok(QueryResult::default())
    }

    /// Roles named by `IN ROLE` of a `CREATE ROLE`: memberships are made after the role exists.
    pub(crate) fn add_role_memberships(&mut self, name: &str, value: &str) -> Result<()> {
        let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(value) else { return Ok(()) };
        for (field, member_is_new) in [("in role", true), ("role", false), ("user", false), ("admin", false)] {
            let Some(list) = map.get(field).and_then(|v| v.as_str()) else { continue };
            for other in list.split(',').filter(|s| !s.is_empty()) {
                if !self.catalog_has("role", other) {
                    return Err(Error::catalog(format!("role \"{other}\" does not exist")));
                }
                let (member, group) = if member_is_new { (name, other) } else { (other, name) };
                self.catalog_set("member", &format!("{member}|{group}"), Some(String::new()))?;
            }
        }
        Ok(())
    }

    /// `SET ROLE name` / `SET ROLE NONE`.
    pub(crate) fn exec_set_role(&mut self, role: Option<&str>) -> Result<QueryResult> {
        match role {
            None => {
                self.settings.remove("role");
            }
            Some(r) => {
                if !r.eq_ignore_ascii_case("current_user") && !r.eq_ignore_ascii_case("session_user") {
                    if !self.catalog_has("role", r) {
                        // no roles at all: the single built-in user, which may name itself
                        return Err(Error::catalog(format!("role \"{r}\" does not exist")));
                    }
                    self.settings.insert("role".into(), r.to_string());
                }
            }
        }
        Ok(QueryResult::default())
    }

    /// The roles whose rights `role` has: itself, the groups it belongs to (and theirs), and `public`.
    fn effective_roles(&self, role: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut todo = vec![key(role)];
        while let Some(r) = todo.pop() {
            if out.insert(r.clone()) {
                for (k, n) in self.catalog.keys() {
                    if k == "member" {
                        if let Some((m, g)) = n.split_once('|') {
                            if m == r {
                                todo.push(g.to_string());
                            }
                        }
                    }
                }
            }
        }
        out.insert("public".to_string());
        out
    }

    // ---- privileges ----

    fn privileges_of(&self, grantee: &str, kind: &str, object: &str) -> BTreeSet<String> {
        self.catalog
            .get(&("grant".to_string(), format!("{}|{kind}:{}", key(grantee), key(object))))
            .map(|v| v.split(',').filter(|s| !s.is_empty()).map(String::from).collect())
            .unwrap_or_default()
    }

    /// `GRANT` and `REVOKE`.
    pub(crate) fn exec_grant(&mut self, g: &GrantDef) -> Result<QueryResult> {
        for grantee in &g.grantees {
            if !grantee.eq_ignore_ascii_case("public") && !self.catalog_has("role", grantee) {
                return Err(Error::catalog(format!("role \"{grantee}\" does not exist")));
            }
        }
        if g.object_kind.is_empty() {
            // GRANT group TO member
            for group in &g.objects {
                if !self.catalog_has("role", group) {
                    return Err(Error::catalog(format!("role \"{group}\" does not exist")));
                }
                for member in &g.grantees {
                    let name = format!("{member}|{group}");
                    self.catalog_set("member", &name, if g.revoke { None } else { Some(String::new()) })?;
                }
            }
            return Ok(QueryResult::default());
        }
        let (kind, objects): (&str, Vec<String>) = match g.object_kind.as_str() {
            "all_tables" | "all_sequences" => {
                let prefix = |s: &str| if s.eq_ignore_ascii_case("public") { String::new() } else { format!("{}.", key(s)) };
                let mut objs = Vec::new();
                for schema in &g.objects {
                    let p = prefix(schema);
                    for t in self.table_names() {
                        let k = key(t);
                        let in_schema = if p.is_empty() { !k.contains('.') } else { k.starts_with(&p) };
                        if in_schema {
                            objs.push(t.to_string());
                        }
                    }
                }
                ("table", objs)
            }
            "table" => {
                for o in &g.objects {
                    if self.table(o).is_none() && self.view_sql(o).is_none() && !self.is_known_object(o) {
                        return Err(Error::catalog(format!("relation \"{o}\" does not exist")));
                    }
                }
                ("table", g.objects.clone())
            }
            other => (other, g.objects.clone()),
        };
        let all: Vec<String> = if kind == "table" { TABLE_PRIVILEGES.iter().map(|s| s.to_string()).collect() } else { vec!["all".into()] };
        let mut wanted: Vec<String> = Vec::new();
        for p in &g.privileges {
            if p == "all" {
                wanted.extend(all.iter().cloned());
            } else {
                wanted.push(p.clone());
            }
        }
        for obj in &objects {
            for grantee in &g.grantees {
                let mut have = self.privileges_of(grantee, kind, obj);
                for p in &wanted {
                    if g.revoke {
                        have.remove(p);
                    } else {
                        have.insert(p.clone());
                    }
                }
                let name = format!("{}|{kind}:{}", key(grantee), key(obj));
                let value = if have.is_empty() { None } else { Some(have.into_iter().collect::<Vec<_>>().join(",")) };
                self.catalog_set("grant", &name, value)?;
            }
        }
        Ok(QueryResult::default())
    }

    fn is_known_object(&self, name: &str) -> bool {
        self.sequences().iter().any(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// Fails with "permission denied" unless the current role may use `table` in the way `privilege` says.
    pub(crate) fn require_privilege(&self, table: &str, privilege: &str) -> Result<()> {
        if self.priv_suspended.load(Ordering::Relaxed) > 0 {
            return Ok(());
        }
        let Some(role) = self.settings.get("role") else { return Ok(()) };
        if role.is_empty() || self.is_superuser(role) || table.starts_with("information_schema.") || table.starts_with("performance_schema.") {
            return Ok(());
        }
        if self.owns(role, table) {
            return Ok(());
        }
        for r in self.effective_roles(role) {
            if self.is_superuser(&r) {
                return Ok(());
            }
            let have = self.privileges_of(&r, "table", table);
            if have.contains(privilege) {
                return Ok(());
            }
        }
        Err(Error::runtime(format!("permission denied for table {table}")))
    }

    /// The function that does the work of a user-defined operator with this many operands.
    pub(crate) fn operator_function(&self, sym: &str, operands: usize) -> Option<String> {
        self.catalog.get(&("operator".to_string(), key(&format!("{sym}/{operands}")))).cloned()
    }

    /// Notes who owns a table or view just made: the current role, if one is set (none otherwise).
    pub(crate) fn record_owner(&mut self, name: &str) {
        let role = self.settings.get("role").filter(|r| !r.is_empty()).cloned();
        let had = self.catalog_has("owner", name);
        if role.is_some() || had {
            let _ = self.catalog_set("owner", name, role);
        }
    }

    /// Does `role` own `table`, or belong to a role that does?
    fn owns(&self, role: &str, table: &str) -> bool {
        match self.catalog.get(&("owner".to_string(), key(table))) {
            Some(owner) => self.effective_roles(role).contains(&key(owner)),
            None => false,
        }
    }

    /// Runs `f` without privilege checks (the body of a view runs with its owner's rights).
    pub(crate) fn without_privileges<T>(&self, f: impl FnOnce() -> T) -> T {
        self.priv_suspended.fetch_add(1, Ordering::Relaxed);
        let out = f();
        self.priv_suspended.fetch_sub(1, Ordering::Relaxed);
        out
    }
}

// ---- row-level security ----

thread_local! {
    /// Tables whose policies are being evaluated, to stop policies that read each other.
    static RLS_ACTIVE: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) struct RlsGuard;

impl Drop for RlsGuard {
    fn drop(&mut self) {
        RLS_ACTIVE.with(|a| {
            a.borrow_mut().pop();
        });
    }
}

/// Marks `table` as having its policies evaluated; fails if it already is (the policies lead back to themselves).
pub(crate) fn rls_enter(table: &str) -> Result<RlsGuard> {
    let k = key(table);
    if RLS_ACTIVE.with(|a| a.borrow().contains(&k)) {
        return Err(Error::runtime(format!("infinite recursion detected in policy for relation \"{table}\"")));
    }
    RLS_ACTIVE.with(|a| a.borrow_mut().push(k));
    Ok(RlsGuard)
}

#[derive(serde::Deserialize)]
struct Policy {
    #[serde(default = "yes")]
    permissive: bool,
    #[serde(default)]
    command: String,
    #[serde(default)]
    roles: Vec<String>,
    using: Option<String>,
    check: Option<String>,
}

fn yes() -> bool {
    true
}

/// `CURRENT_USER`, `SESSION_USER` and `USER` as the quoted name of the role.
fn with_role(text: &str, role: &str) -> String {
    let quoted = format!("'{}'", role.replace('\'', "''"));
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut in_quote = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            in_quote = !in_quote;
        }
        if !in_quote && (c.is_alphabetic() || c == '_') {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if matches!(word.to_ascii_lowercase().as_str(), "current_user" | "session_user") {
                out.push_str(&quoted);
            } else {
                out.push_str(&word);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

impl Database {
    /// `ALTER TABLE t ENABLE | DISABLE | FORCE | NO FORCE ROW LEVEL SECURITY`.
    pub(crate) fn exec_set_rls(&mut self, table: &str, enable: Option<bool>, force: Option<bool>) -> Result<QueryResult> {
        let real = self.table_checked(table)?.name().to_string();
        let (mut on, mut forced) = self.rls_state(&real);
        if let Some(e) = enable {
            on = e;
        }
        if let Some(f) = force {
            forced = f;
        }
        let value = format!("{}{}", u8::from(on), u8::from(forced));
        self.catalog_set("rls", &real, Some(value))?;
        Ok(QueryResult::default())
    }

    fn rls_state(&self, table: &str) -> (bool, bool) {
        match self.catalog.get(&("rls".to_string(), key(table))) {
            Some(v) => (v.starts_with('1'), v.ends_with('1')),
            None => (false, false),
        }
    }

    /// The condition the policies of the current role put on `command` ("select", "insert", "update", "delete") of
    /// `table`: `None` when nothing is restricted (security is off for the table, or no role is chosen, or the role is
    /// a superuser); a condition that is false when no policy lets the role in. With `check`, the `WITH CHECK` side.
    pub(crate) fn rls_expr(&self, table: &str, command: &str, check: bool) -> Result<Option<crate::sql::ast::Expr>> {
        use crate::sql::ast::{BinOp, Expr};
        use crate::value::Value;
        if self.catalog.is_empty() || !self.rls_state(table).0 {
            return Ok(None);
        }
        let Some(role) = self.settings.get("role").filter(|r| !r.is_empty()) else { return Ok(None) };
        if self.is_superuser(role) {
            return Ok(None);
        }
        let roles = self.effective_roles(role);
        let prefix = format!("{}|", key(table));
        let mut permissive: Vec<Expr> = Vec::new();
        let mut restrictive: Vec<Expr> = Vec::new();
        for ((k, n), v) in &self.catalog {
            if k != "policy" || !n.starts_with(&prefix) {
                continue;
            }
            let p: Policy = serde_json::from_str(v).map_err(|e| Error::runtime(format!("a policy is damaged: {e}")))?;
            if !(p.command == "all" || p.command == command) {
                continue;
            }
            if !p.roles.is_empty() && !p.roles.iter().any(|r| roles.contains(r) || r == "public") {
                continue;
            }
            let text = if check { p.check.clone().or(p.using.clone()) } else { p.using.clone() };
            let expr = match text {
                Some(t) => crate::sql::parse_expression(&with_role(&t, role))?,
                None => Expr::Literal(Value::Bool(true)),
            };
            if p.permissive { permissive.push(expr) } else { restrictive.push(expr) }
        }
        let or = |v: Vec<Expr>| v.into_iter().reduce(|a, b| Expr::Binary { op: BinOp::Or, left: Box::new(a), right: Box::new(b) });
        let mut all = vec![or(permissive).unwrap_or(Expr::Literal(Value::Bool(false)))];
        all.extend(restrictive);
        Ok(all.into_iter().reduce(|a, b| Expr::Binary { op: BinOp::And, left: Box::new(a), right: Box::new(b) }))
    }

    /// Fails when a row about to be written does not satisfy the `WITH CHECK` of the policies.
    pub(crate) fn rls_check(&self, table: &str, command: &str, ops: &[crate::storage::Op]) -> Result<()> {
        let Some(expr) = self.rls_expr(table, command, true)? else { return Ok(()) };
        let _recursion = rls_enter(table)?;
        let t = self.table_checked(table)?;
        let scope = super::dml::table_scope(t.schema());
        let bound = super::bind::Binder::new(self, &scope, 0).bind(&expr)?;
        for op in ops {
            let row = match op {
                crate::storage::Op::Insert { row, .. } | crate::storage::Op::Update { row, .. } => row,
                _ => continue,
            };
            if !bound.eval_bool(row)? {
                return Err(Error::constraint(format!("new row violates row-level security policy for table \"{}\"", t.name())));
            }
        }
        Ok(())
    }
}
