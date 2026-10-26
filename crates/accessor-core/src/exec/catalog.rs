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
//!   which is the case until a script runs `SET ROLE`. Tables have no owners.

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

    /// `CREATE` / `DROP` of a schema, type, extension or role.
    pub(crate) fn exec_catalog(&mut self, c: &CatalogChange) -> Result<QueryResult> {
        for name in &c.names {
            let what = describe(c.kind);
            match &c.value {
                Some(value) => {
                    if self.catalog_has(c.kind, name) && !c.replace {
                        return Err(Error::catalog(format!("{what} \"{name}\" already exists")));
                    }
                    if c.kind == "type" && self.catalog_has("type", name) {
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

    fn drop_catalog_object(&mut self, kind: &str, name: &str, cascade: bool) -> Result<()> {
        match kind {
            "schema" => {
                let prefix = format!("{}.", key(name));
                let tables: Vec<String> =
                    self.table_names().into_iter().filter(|t| key(t).starts_with(&prefix)).map(String::from).collect();
                let views: Vec<String> =
                    self.view_names().into_iter().filter(|t| key(t).starts_with(&prefix)).map(String::from).collect();
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

    fn is_superuser(&self, role: &str) -> bool {
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
        if role.is_empty() || self.is_superuser(role) || table.starts_with("information_schema.") {
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

    /// Runs `f` without privilege checks (the body of a view runs with its owner's rights).
    pub(crate) fn without_privileges<T>(&self, f: impl FnOnce() -> T) -> T {
        self.priv_suspended.fetch_add(1, Ordering::Relaxed);
        let out = f();
        self.priv_suspended.fetch_sub(1, Ordering::Relaxed);
        out
    }
}
