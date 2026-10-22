//! Query view: SQL editor with a result grid.

use std::time::Instant;

use accessor_core::sql::{ast::Statement, parse_script};
use accessor_core::{Database, Row, Value};
use egui::{Key, RichText, TextEdit};
use egui_extras::{Column, TableBuilder};

use crate::ui::{display, Event};

enum Outcome {
    None,
    Rows { columns: Vec<String>, rows: Vec<Row>, ms: f64 },
    Affected { n: usize, ms: f64 },
    Error(String),
}

pub struct QueryTab {
    /// Set when the tab edits a saved query.
    pub name: Option<String>,
    pub sql: String,
    outcome: Outcome,
    save_as: String,
    saving: bool,
    /// Text typed for each `@parameter` of the script, in declaration order.
    params: Vec<(String, String)>,
    /// The SQL `params` was derived from, so it is re-parsed only when edited.
    params_for: String,
}

impl QueryTab {
    pub fn new(name: Option<String>, sql: String) -> Self {
        QueryTab {
            name,
            sql,
            outcome: Outcome::None,
            save_as: String::new(),
            saving: false,
            params: Vec::new(),
            params_for: String::new(),
        }
    }

    pub fn results(&self) -> Option<(&[String], &[Row])> {
        match &self.outcome {
            Outcome::Rows { columns, rows, .. } => Some((columns, rows)),
            _ => None,
        }
    }

    pub fn run(&mut self, db: &mut Database, ev: &mut Vec<Event>) {
        let start = Instant::now();
        let stmts = match parse_script(&self.sql) {
            Ok(s) if !s.is_empty() => s,
            Ok(_) => return,
            Err(e) => {
                self.outcome = Outcome::Error(e.to_string());
                return;
            }
        };
        let mut changed = false;
        let mut last = None;
        if matches!(stmts.first(), Some(Statement::Parameters(_))) {
            // A parameterised script runs as a whole, with the values typed above.
            changed = stmts.iter().any(|s| !matches!(s, Statement::Select(_) | Statement::Parameters(_)));
            let pairs: Vec<(&str, Value)> = self.params.iter().map(|(n, t)| (n.as_str(), infer(t))).collect();
            match db.execute_with_params(&self.sql, &pairs) {
                Ok(r) => last = Some(r),
                Err(e) => self.outcome = Outcome::Error(e.to_string()),
            }
            if last.is_none() {
                if changed {
                    ev.push(Event::Changed);
                }
                return;
            }
        }
        let plain: &[Statement] = if last.is_none() { &stmts } else { &[] };
        for st in plain {
            changed |= !matches!(st, Statement::Select(_));
            match db.execute_statement(st) {
                Ok(r) => last = Some(r),
                Err(e) => {
                    self.outcome = Outcome::Error(e.to_string());
                    if changed {
                        ev.push(Event::Changed);
                    }
                    return;
                }
            }
        }
        let ms = start.elapsed().as_secs_f64() * 1e3;
        self.outcome = match last {
            Some(r) if !r.columns.is_empty() => Outcome::Rows { columns: r.columns, rows: r.rows, ms },
            Some(r) => Outcome::Affected { n: r.affected, ms },
            None => Outcome::None,
        };
        if changed {
            ev.push(Event::Changed);
        }
    }

    /// Text boxes for the `PARAMETERS` the script declares.
    fn parameter_inputs(&mut self, ui: &mut egui::Ui) {
        if self.params_for != self.sql {
            self.params_for = self.sql.clone();
            let declared: Vec<String> = match parse_script(&self.sql).ok().and_then(|s| s.into_iter().next()) {
                Some(Statement::Parameters(d)) => d.into_iter().map(|p| p.name).collect(),
                _ => Vec::new(),
            };
            // Keep what was typed for parameters that are still declared.
            self.params = declared
                .into_iter()
                .map(|n| {
                    let text = self.params.iter().find(|(o, _)| *o == n).map(|(_, t)| t.clone()).unwrap_or_default();
                    (n, text)
                })
                .collect();
        }
        if self.params.is_empty() {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Parameters:").strong());
            for (name, text) in &mut self.params {
                ui.label(format!("@{name}"));
                ui.add(TextEdit::singleline(text).desired_width(120.0));
            }
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, db: &mut Database, ev: &mut Vec<Event>) {
        let id = ("query", self.name.clone());
        ui.push_id(id, |ui| self.show(ui, db, ev));
    }

    fn show(&mut self, ui: &mut egui::Ui, db: &mut Database, ev: &mut Vec<Event>) {
        let run_shortcut = ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
        let mut run = run_shortcut;
        ui.horizontal(|ui| {
            if ui.button("Run  (Ctrl+Enter)").clicked() {
                run = true;
            }
            match &self.name {
                Some(n) => {
                    if ui.button(format!("Save '{n}'")).clicked() {
                        let name = n.clone();
                        let sql = self.sql.trim().trim_end_matches(';').to_string();
                        let old = db.view_sql(&name).map(str::to_string);
                        let res = db
                            .execute(&format!("DROP VIEW [{name}]"))
                            .and_then(|_| db.execute(&format!("CREATE VIEW [{name}] AS {sql}")));
                        // A rejected edit must not cost the query it replaces.
                        if let (Err(_), Some(old)) = (&res, old) {
                            let _ = db.execute(&format!("CREATE VIEW [{name}] AS {old}"));
                        }
                        match res {
                            Ok(_) => {
                                ev.push(Event::Status(format!("Saved query '{name}'")));
                                ev.push(Event::Changed);
                            }
                            Err(e) => ev.push(Event::Error(e.to_string())),
                        }
                    }
                }
                None => {
                    if ui.button("Save as query...").clicked() {
                        self.saving = true;
                    }
                }
            }
            if self.saving {
                ui.add(TextEdit::singleline(&mut self.save_as).hint_text("query name").desired_width(160.0));
                if ui.button("Save").clicked() && !self.save_as.trim().is_empty() {
                    let name = self.save_as.trim().replace(']', "");
                    let sql = self.sql.trim().trim_end_matches(';').to_string();
                    match db.execute(&format!("CREATE VIEW [{name}] AS {sql}")) {
                        Ok(_) => {
                            self.name = Some(name.clone());
                            self.saving = false;
                            ev.push(Event::Status(format!("Saved query '{name}'")));
                            ev.push(Event::Changed);
                        }
                        Err(e) => ev.push(Event::Error(e.to_string())),
                    }
                }
                if ui.button("Cancel").clicked() {
                    self.saving = false;
                }
            }
        });

        ui.add(
            TextEdit::multiline(&mut self.sql)
                .code_editor()
                .desired_rows(7)
                .desired_width(f32::INFINITY)
                .hint_text("SELECT ... FROM ..."),
        );
        self.parameter_inputs(ui);
        if run {
            self.run(db, ev);
        }
        ui.separator();

        match &self.outcome {
            Outcome::None => {
                ui.label(RichText::new("Write a statement and press Run.").weak());
            }
            Outcome::Error(e) => {
                ui.colored_label(egui::Color32::from_rgb(180, 30, 30), e);
            }
            Outcome::Affected { n, ms } => {
                ui.label(format!("{n} row(s) affected in {ms:.2} ms"));
            }
            Outcome::Rows { columns, rows, ms } => {
                ui.label(RichText::new(format!("{} row(s) in {ms:.2} ms", rows.len())).weak());
                result_grid(ui, "query_results", columns, rows);
            }
        }
    }
}

pub fn result_grid(ui: &mut egui::Ui, id: &str, columns: &[String], rows: &[Row]) {
    ui.push_id(id, |ui| {
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .columns(Column::initial(130.0).at_least(50.0).clip(true), columns.len().max(1))
            .header(24.0, |mut h| {
                for c in columns {
                    h.col(|ui| {
                        ui.label(RichText::new(c).strong());
                    });
                }
            })
            .body(|body| {
                body.rows(22.0, rows.len(), |mut row| {
                    let r = &rows[row.index()];
                    for v in r {
                        row.col(|ui| {
                            ui.add(egui::Label::new(display(v)).truncate());
                        });
                    }
                });
            });
    });
}

/// Reads text typed for a parameter: blank is Null, then a number if it
/// looks like one, otherwise text.
fn infer(text: &str) -> Value {
    let t = text.trim();
    if t.is_empty() {
        Value::Null
    } else if let Some(n) = Value::parse_number(t) {
        n
    } else {
        Value::text(text)
    }
}
