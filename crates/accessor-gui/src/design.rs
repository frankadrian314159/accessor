//! Design view (edit the structure of an existing table) and the
//! "new table" designer.

use accessor_core::{ArrayOf, DataType, Database, RefAction};
use egui::RichText;

use crate::ui::{bracket, Event};

/// The drop-down scrolls: there are the thirteen types and an array of each.
const TYPE_LIST_HEIGHT: f32 = 340.0;

const TYPES: [DataType; 27] = [
    DataType::Integer,
    DataType::Bignum,
    DataType::Float,
    DataType::Double,
    DataType::Decimal,
    DataType::Currency,
    DataType::Rational,
    DataType::Text,
    DataType::YesNo,
    DataType::DateTime,
    DataType::TimestampTz,
    DataType::Interval,
    DataType::Json,
    DataType::Array(ArrayOf::Any),
    DataType::Array(ArrayOf::Integer),
    DataType::Array(ArrayOf::Bignum),
    DataType::Array(ArrayOf::Float),
    DataType::Array(ArrayOf::Double),
    DataType::Array(ArrayOf::Decimal),
    DataType::Array(ArrayOf::Currency),
    DataType::Array(ArrayOf::Rational),
    DataType::Array(ArrayOf::Text),
    DataType::Array(ArrayOf::YesNo),
    DataType::Array(ArrayOf::DateTime),
    DataType::Array(ArrayOf::TimestampTz),
    DataType::Array(ArrayOf::Interval),
    DataType::Array(ArrayOf::Json),
];

/// Types offered for a new field, Calculated after the thirteen plain ones (so it is in view without scrolling) and the
/// arrays after it. An existing field can only be converted to the ordinary types (`TYPES`); a field becomes
/// calculated by being created that way.
const NEW_TYPES: [DataType; 28] = [
    DataType::Integer,
    DataType::Bignum,
    DataType::Float,
    DataType::Double,
    DataType::Decimal,
    DataType::Currency,
    DataType::Rational,
    DataType::Text,
    DataType::YesNo,
    DataType::DateTime,
    DataType::TimestampTz,
    DataType::Interval,
    DataType::Json,
    DataType::Calculated,
    DataType::Array(ArrayOf::Any),
    DataType::Array(ArrayOf::Integer),
    DataType::Array(ArrayOf::Bignum),
    DataType::Array(ArrayOf::Float),
    DataType::Array(ArrayOf::Double),
    DataType::Array(ArrayOf::Decimal),
    DataType::Array(ArrayOf::Currency),
    DataType::Array(ArrayOf::Rational),
    DataType::Array(ArrayOf::Text),
    DataType::Array(ArrayOf::YesNo),
    DataType::Array(ArrayOf::DateTime),
    DataType::Array(ArrayOf::TimestampTz),
    DataType::Array(ArrayOf::Interval),
    DataType::Array(ArrayOf::Json),
];

pub struct Design {
    pub table: String,
    /// Field being renamed and the text typed so far.
    renaming: Option<(String, String)>,
    /// Calculated field whose formula is being edited, and the text typed so far.
    editing_formula: Option<(String, String)>,
    /// Why the last formula edit was refused.
    formula_error: Option<String>,
    /// An ordinary field and the formula it is about to be replaced by, awaiting confirmation.
    confirm_convert: Option<(String, String)>,
    add_formula: String,
    /// Field awaiting confirmation before it is dropped.
    dropping: Option<String>,
    add_name: String,
    add_type: DataType,
    add_required: bool,
    rel_field: usize,
    rel_table: String,
    rel_col: usize,
    rel_delete: RefAction,
    rel_update: RefAction,
}

impl Design {
    pub fn new(table: String) -> Self {
        Design {
            table,
            renaming: None,
            editing_formula: None,
            formula_error: None,
            confirm_convert: None,
            add_formula: String::new(),
            dropping: None,
            add_name: String::new(),
            add_type: DataType::Text,
            add_required: false,
            rel_field: 0,
            rel_table: String::new(),
            rel_col: 0,
            rel_delete: RefAction::Restrict,
            rel_update: RefAction::Restrict,
        }
    }

    fn alter(&self, ev: &mut Vec<Event>, action: String, ok: String) {
        ev.push(Event::Execute { sql: format!("ALTER TABLE {} {action}", bracket(&self.table)), ok });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, db: &Database, ev: &mut Vec<Event>) {
        let id = ("design", self.table.clone());
        ui.push_id(id, |ui| self.show(ui, db, ev));
    }

    fn show(&mut self, ui: &mut egui::Ui, db: &Database, ev: &mut Vec<Event>) {
        let Some(t) = db.table(&self.table) else {
            ui.label("This table no longer exists.");
            return;
        };
        let schema = t.schema();
        ui.heading(&schema.name);
        ui.label(RichText::new(format!("{} records", t.len())).weak());
        ui.add_space(6.0);
        let only_column = schema.columns.len() == 1;
        let mut actions: Vec<(String, String)> = Vec::new();
        egui::Grid::new(("design", &self.table)).striped(true).spacing([18.0, 6.0]).show(ui, |ui| {
            for h in ["Field name", "Data type", "Required", "Primary key", "Default / formula", ""] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for c in &schema.columns {
                match &mut self.renaming {
                    Some((from, text)) if from.eq_ignore_ascii_case(&c.name) => {
                        let r = ui.add(egui::TextEdit::singleline(text).desired_width(140.0));
                        let commit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if !r.has_focus() && !commit && !cancel {
                            r.request_focus();
                        }
                        if commit {
                            let to = text.trim().to_string();
                            if !to.is_empty() && to != c.name {
                                actions.push((
                                    format!("RENAME COLUMN {} TO {}", bracket(&c.name), bracket(&to)),
                                    format!("Renamed '{}' to '{to}'", c.name),
                                ));
                            }
                            self.renaming = None;
                        } else if cancel {
                            self.renaming = None;
                        }
                    }
                    _ => {
                        ui.label(&c.name);
                    }
                }
                if c.autonumber {
                    ui.label("AutoNumber");
                } else {
                    let mut ty = c.data_type;
                    // Any field except a key can be (or become) calculated.
                    let can_calculate = !c.primary_key;
                    egui::ComboBox::from_id_salt(("design_ty", &self.table, &c.name)).height(TYPE_LIST_HEIGHT)
                        .selected_text(ty.to_string())
                        .show_ui(ui, |ui| {
                            for (i, t) in TYPES.into_iter().enumerate() {
                                if i == 13 && can_calculate {
                                    ui.selectable_value(&mut ty, DataType::Calculated, DataType::Calculated.to_string());
                                }
                                ui.selectable_value(&mut ty, t, t.to_string());
                            }
                        });
                    if ty != c.data_type {
                        if ty == DataType::Calculated {
                            // Needs a formula, and the field's values will be replaced: ask for it.
                            self.formula_error = None;
                            self.confirm_convert = None;
                            self.editing_formula = Some((c.name.clone(), String::new()));
                        } else {
                            actions.push((
                                format!("ALTER COLUMN {} {}", bracket(&c.name), ty.sql_name()),
                                format!("'{}' is now {ty}", c.name),
                            ));
                        }
                    }
                }
                ui.label(if c.not_null { "Yes" } else { "No" });
                ui.label(if c.primary_key { "Yes" } else { "" });
                let editing_this = matches!(&self.editing_formula, Some((f, _)) if f.eq_ignore_ascii_case(&c.name));
                if let Some(expr) = c.expression() {
                    if editing_this {
                        if let Some(formula) = self.formula_box(ui, db, &c.name, false) {
                            if formula != expr {
                                actions.push((
                                    format!("ALTER COLUMN {} CALCULATED AS ({formula})", bracket(&c.name)),
                                    format!("Changed the formula of '{}'", c.name),
                                ));
                            }
                        }
                    } else {
                        ui.label(format!("= {expr}"));
                    }
                } else if editing_this {
                    // An ordinary field becoming calculated: ask before its values are replaced.
                    if let Some(formula) = self.formula_box(ui, db, &c.name, true) {
                        self.confirm_convert = Some((c.name.clone(), formula));
                    }
                } else {
                    ui.label(c.default.as_ref().map(ToString::to_string).unwrap_or_default());
                }
                ui.horizontal(|ui| {
                    if self.dropping.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(&c.name)) {
                        ui.label(RichText::new("Delete field and its data?").color(egui::Color32::from_rgb(180, 30, 30)));
                        if ui.small_button("Yes").clicked() {
                            actions.push((format!("DROP COLUMN {}", bracket(&c.name)), format!("Deleted field '{}'", c.name)));
                            self.dropping = None;
                        }
                        if ui.small_button("No").clicked() {
                            self.dropping = None;
                        }
                    } else {
                        if ui.small_button("Rename").clicked() {
                            self.renaming = Some((c.name.clone(), c.name.clone()));
                        }
                        if let Some(expr) = c.expression() {
                            if ui.small_button("Formula").clicked() {
                                self.formula_error = None;
                                self.editing_formula = Some((c.name.clone(), expr.to_string()));
                            }
                        } else if !c.primary_key && !c.autonumber && ui.small_button("Calculate...").on_hover_text("Replace this field's values with a formula").clicked() {
                            self.formula_error = None;
                            self.confirm_convert = None;
                            self.editing_formula = Some((c.name.clone(), String::new()));
                        }
                        if ui.add_enabled(!only_column, egui::Button::new("Delete").small()).clicked() {
                            self.dropping = Some(c.name.clone());
                        }
                    }
                });
                ui.end_row();
            }
        });
        if let Some((field, formula)) = self.confirm_convert.clone() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Replace every value of '{field}' with ({formula})? The current values are lost.")).color(egui::Color32::from_rgb(180, 30, 30)));
                if ui.small_button("Yes").clicked() {
                    actions.push((
                        format!("ALTER COLUMN {} CALCULATED AS ({formula})", bracket(&field)),
                        format!("'{field}' is now a calculated field"),
                    ));
                    self.confirm_convert = None;
                }
                if ui.small_button("No").clicked() {
                    self.confirm_convert = None;
                }
            });
        }
        ui.add_space(8.0);
        if let Some(e) = &self.formula_error {
            ui.colored_label(egui::Color32::from_rgb(180, 30, 30), e);
            ui.add_space(4.0);
        }
        // Judge a typed formula as it is entered, so the problem is visible beside the box.
        let live_error = if self.add_type == DataType::Calculated && !self.add_formula.trim().is_empty() {
            db.check_formula(&self.table, &self.add_name, self.add_formula.trim()).err().map(|e| e.to_string())
        } else {
            None
        };
        ui.horizontal(|ui| {
            ui.label("Add field:");
            ui.add(egui::TextEdit::singleline(&mut self.add_name).desired_width(140.0).hint_text("name"));
            egui::ComboBox::from_id_salt(("design_add_ty", &self.table)).height(TYPE_LIST_HEIGHT)
                .selected_text(self.add_type.to_string())
                .show_ui(ui, |ui| {
                    for t in NEW_TYPES {
                        ui.selectable_value(&mut self.add_type, t, t.to_string());
                    }
                });
            let calculated = self.add_type == DataType::Calculated;
            if calculated {
                ui.label("=");
                ui.add(egui::TextEdit::singleline(&mut self.add_formula).desired_width(220.0).hint_text("e.g. Qty * Price"));
            } else {
                ui.checkbox(&mut self.add_required, "Required");
            }
            // A required field needs a value for existing records, which only a default can supply.
            let blocked = self.add_required && !calculated && !t.is_empty();
            let name = self.add_name.trim().to_string();
            let formula = self.add_formula.trim().to_string();
            let ready = !name.is_empty() && !blocked && (!calculated || (!formula.is_empty() && live_error.is_none()));
            if ui.add_enabled(ready, egui::Button::new("Add")).clicked() {
                let mut action = if calculated {
                    format!("ADD COLUMN {} CALCULATED AS ({formula})", bracket(&name))
                } else {
                    format!("ADD COLUMN {} {}", bracket(&name), self.add_type.sql_name())
                };
                if self.add_required && !calculated {
                    action.push_str(" NOT NULL");
                }
                self.add_formula.clear();
                actions.push((action, format!("Added field '{name}'")));
                self.add_name.clear();
                self.add_required = false;
            }
            if blocked {
                ui.label(RichText::new("Not allowed on a table that already has records.").weak());
            }
        });
        if let Some(e) = &live_error {
            ui.colored_label(egui::Color32::from_rgb(180, 30, 30), e);
        }
        for (action, ok) in actions {
            self.alter(ev, action, ok);
        }
        ui.add_space(14.0);
        ui.label(RichText::new("Indexes").strong());
        egui::Grid::new(("design_ix", &self.table)).striped(true).spacing([18.0, 6.0]).show(ui, |ui| {
            for h in ["Index name", "Fields", "Unique"] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for ix in &schema.indexes {
                ui.label(&ix.name);
                ui.label(ix.columns.iter().map(|&c| schema.columns[c].name.as_str()).collect::<Vec<_>>().join(", "));
                ui.label(if ix.unique { "Yes" } else { "No" });
                ui.end_row();
            }
        });
        ui.add_space(14.0);
        self.relationships(ui, db, ev);
        ui.add_space(10.0);
        ui.label(
            RichText::new("Indexed fields cannot be deleted until their index is dropped (DROP INDEX). Type changes that lose data are refused.")
                .weak(),
        );
    }

    /// The formula box shown in place of a field's default. Returns the formula once
    /// Enter is pressed on one that passes the check; a rejected one stays open with
    /// its reason shown below the list of fields.
    fn formula_box(&mut self, ui: &mut egui::Ui, db: &Database, name: &str, convert: bool) -> Option<String> {
        let (_, text) = self.editing_formula.as_mut()?;
        let r = ui.add(egui::TextEdit::singleline(text).desired_width(220.0).hint_text("e.g. First & ' ' & Last"));
        let commit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if !r.has_focus() && !commit && !cancel {
            r.request_focus();
        }
        if commit {
            let formula = text.trim().to_string();
            let checked = if convert {
                db.check_conversion(&self.table, name, &formula)
            } else {
                db.check_formula(&self.table, name, &formula)
            };
            match checked {
                Ok(()) => {
                    self.formula_error = None;
                    self.editing_formula = None;
                    Some(formula)
                }
                Err(e) => {
                    self.formula_error = Some(e.to_string());
                    None
                }
            }
        } else {
            if cancel {
                self.formula_error = None;
                self.editing_formula = None;
            }
            None
        }
    }

    /// Lists this table's relationships and offers a form to add one.
    fn relationships(&mut self, ui: &mut egui::Ui, db: &Database, ev: &mut Vec<Event>) {
        let Some(t) = db.table(&self.table) else { return };
        let schema = t.schema();
        let field = |s: &accessor_core::TableSchema, cols: &[usize]| {
            cols.iter().map(|&c| s.columns[c].name.as_str()).collect::<Vec<_>>().join(", ")
        };
        ui.label(RichText::new("Relationships").strong());
        if schema.foreign_keys.is_empty() {
            ui.label(RichText::new("(none) - a field can refer to a record in another table").weak());
        } else {
            let mut drop = None;
            egui::Grid::new(("design_fk", &self.table)).striped(true).spacing([18.0, 6.0]).show(ui, |ui| {
                for h in ["Name", "Field", "Refers to", "On delete", "On key change", ""] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                for fk in &schema.foreign_keys {
                    let target = db.table(&fk.ref_table).map_or_else(
                        || fk.ref_table.clone(),
                        |p| format!("{} ({})", p.name(), field(p.schema(), &fk.ref_columns)),
                    );
                    ui.label(&fk.name);
                    ui.label(field(schema, &fk.columns));
                    ui.label(target);
                    ui.label(fk.on_delete.sql().to_lowercase());
                    ui.label(fk.on_update.sql().to_lowercase());
                    if ui.small_button("Delete").clicked() {
                        drop = Some(fk.name.clone());
                    }
                    ui.end_row();
                }
            });
            if let Some(name) = drop {
                let action = format!("DROP CONSTRAINT {}", bracket(&name));
                self.alter(ev, action, format!("Deleted relationship '{name}'"));
            }
        }

        // Add form: field -> table (key field) with referential actions.
        let tables = db.table_names();
        self.rel_field = self.rel_field.min(schema.columns.len().saturating_sub(1));
        if !tables.iter().any(|n| n.eq_ignore_ascii_case(&self.rel_table)) {
            self.rel_table = tables.first().map(|s| s.to_string()).unwrap_or_default();
            self.rel_col = 0;
        }
        let parent = db.table(&self.rel_table);
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label("Field");
            egui::ComboBox::from_id_salt(("rel_field", &self.table))
                .selected_text(&schema.columns[self.rel_field].name)
                .show_ui(ui, |ui| {
                    for (i, c) in schema.columns.iter().enumerate() {
                        ui.selectable_value(&mut self.rel_field, i, &c.name);
                    }
                });
            ui.label("refers to");
            let before = self.rel_table.clone();
            egui::ComboBox::from_id_salt(("rel_table", &self.table)).selected_text(&self.rel_table).show_ui(ui, |ui| {
                for n in &tables {
                    ui.selectable_value(&mut self.rel_table, n.to_string(), *n);
                }
            });
            if before != self.rel_table {
                self.rel_col = 0;
            }
            if let Some(p) = parent {
                // Only fields that uniquely identify a record can be referenced.
                let keys: Vec<usize> = (0..p.schema().columns.len())
                    .filter(|&i| p.schema().indexes.iter().any(|ix| ix.unique && ix.columns == [i]))
                    .collect();
                if !keys.contains(&self.rel_col) {
                    self.rel_col = keys.first().copied().unwrap_or(0);
                }
                egui::ComboBox::from_id_salt(("rel_col", &self.table))
                    .selected_text(&p.schema().columns[self.rel_col].name)
                    .show_ui(ui, |ui| {
                        for &i in &keys {
                            ui.selectable_value(&mut self.rel_col, i, &p.schema().columns[i].name);
                        }
                    });
                ui.label("on delete");
                action_combo(ui, ("rel_del", &self.table), &mut self.rel_delete);
                ui.label("on key change");
                action_combo(ui, ("rel_upd", &self.table), &mut self.rel_update);
                if ui.add_enabled(!keys.is_empty(), egui::Button::new("Create relationship")).clicked() {
                    let action = format!(
                        "ADD FOREIGN KEY ({}) REFERENCES {} ({}) ON DELETE {} ON UPDATE {}",
                        bracket(&schema.columns[self.rel_field].name),
                        bracket(p.name()),
                        bracket(&p.schema().columns[self.rel_col].name),
                        self.rel_delete.sql(),
                        self.rel_update.sql(),
                    );
                    self.alter(ev, action, "Created relationship".to_string());
                }
            }
        });
    }
}

/// Drop-down over the referential actions.
fn action_combo(ui: &mut egui::Ui, id: impl std::hash::Hash, value: &mut RefAction) {
    egui::ComboBox::from_id_salt(id).selected_text(value.sql().to_lowercase()).show_ui(ui, |ui| {
        for a in RefAction::ALL {
            ui.selectable_value(value, a, a.sql().to_lowercase());
        }
    });
}

#[derive(Clone)]
pub struct FieldDef {
    pub name: String,
    pub data_type: DataType,
    pub autonumber: bool,
    pub primary_key: bool,
    pub required: bool,
    /// Expression of a `Calculated` field.
    pub formula: String,
}

impl Default for FieldDef {
    fn default() -> Self {
        FieldDef {
            name: String::new(),
            data_type: DataType::Text,
            autonumber: false,
            primary_key: false,
            required: false,
            formula: String::new(),
        }
    }
}

/// Modal editor for a table that does not exist yet.
pub struct NewTable {
    pub name: String,
    pub fields: Vec<FieldDef>,
}

impl NewTable {
    pub fn new() -> Self {
        NewTable {
            name: String::new(),
            fields: vec![FieldDef {
                name: "ID".into(),
                data_type: DataType::Integer,
                autonumber: true,
                primary_key: true,
                required: true,
                formula: String::new(),
            }],
        }
    }

    /// Draws the form; returns `Some(sql)` once the user presses Create.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<String> {
        ui.horizontal(|ui| {
            ui.label("Table name:");
            ui.text_edit_singleline(&mut self.name);
        });
        ui.add_space(6.0);
        let mut remove = None;
        egui::Grid::new("newtable_fields").spacing([10.0, 6.0]).show(ui, |ui| {
            for h in ["Field name", "Data type", "Formula", "AutoNumber", "Primary key", "Required", ""] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for (i, f) in self.fields.iter_mut().enumerate() {
                ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(140.0));
                egui::ComboBox::from_id_salt(("ty", i)).height(TYPE_LIST_HEIGHT)
                    .selected_text(f.data_type.to_string())
                    .show_ui(ui, |ui| {
                        for t in NEW_TYPES {
                            ui.selectable_value(&mut f.data_type, t, t.to_string());
                        }
                    });
                let calculated = f.data_type == DataType::Calculated;
                if calculated {
                    ui.add(egui::TextEdit::singleline(&mut f.formula).desired_width(180.0).hint_text("e.g. Qty * Price"));
                    f.autonumber = false;
                    f.primary_key = false;
                    f.required = false;
                } else {
                    ui.label("");
                }
                if ui.add_enabled(!calculated, egui::Checkbox::new(&mut f.autonumber, "")).changed() && f.autonumber {
                    f.data_type = DataType::Integer;
                }
                ui.add_enabled(!calculated, egui::Checkbox::new(&mut f.primary_key, ""));
                ui.add_enabled(!calculated, egui::Checkbox::new(&mut f.required, ""));
                if ui.small_button("remove").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = remove {
            self.fields.remove(i);
        }
        if ui.button("+ Add field").clicked() {
            self.fields.push(FieldDef::default());
        }
        ui.add_space(8.0);
        // Judge each formula as it is typed (it can only be fully checked once the table exists).
        let formula_errors: Vec<String> = self
            .fields
            .iter()
            .filter(|f| f.data_type == DataType::Calculated && !f.formula.trim().is_empty())
            .filter_map(|f| {
                accessor_core::sql::parse_expression(f.formula.trim()).err().map(|e| {
                    let hint = if f.formula.contains("&&") { " (to join text use a single &)" } else { "" };
                    format!("The formula for '{}' is not valid: {e}{hint}", f.name.trim())
                })
            })
            .collect();
        for e in &formula_errors {
            ui.colored_label(egui::Color32::from_rgb(180, 30, 30), e);
        }
        let valid = !self.name.trim().is_empty()
            && !self.fields.is_empty()
            && self.fields.iter().all(|f| !f.name.trim().is_empty())
            && self.fields.iter().all(|f| f.data_type != DataType::Calculated || !f.formula.trim().is_empty())
            && formula_errors.is_empty();
        let mut out = None;
        ui.horizontal(|ui| {
            if ui.add_enabled(valid, egui::Button::new("Create table")).clicked() {
                out = Some(self.to_sql());
            }
            if !valid {
                ui.label(RichText::new("Name the table and every field (and give each calculated field a formula).").weak());
            }
        });
        out
    }

    fn to_sql(&self) -> String {
        let fields: Vec<String> = self
            .fields
            .iter()
            .map(|f| {
                if f.data_type == DataType::Calculated {
                    return format!("{} CALCULATED AS ({})", bracket(f.name.trim()), f.formula.trim());
                }
                let ty = if f.autonumber { "COUNTER" } else { f.data_type.sql_name() };
                let mut s = format!("{} {ty}", bracket(f.name.trim()));
                if f.primary_key {
                    s.push_str(" PRIMARY KEY");
                } else if f.required {
                    s.push_str(" NOT NULL");
                }
                s
            })
            .collect();
        format!("CREATE TABLE {} ({})", bracket(self.name.trim()), fields.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album() -> Database {
        let mut db = Database::in_memory();
        db.execute("CREATE TABLE Album (ID COUNTER PRIMARY KEY, Name TEXT, Year INTEGER)").unwrap();
        db
    }

    /// Draws one frame and returns the text on screen and the events raised.
    fn frame(
        ctx: &egui::Context,
        design: &mut Design,
        db: &Database,
        events: Vec<egui::Event>,
        time: f64,
    ) -> (Vec<(egui::Pos2, String)>, Vec<Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100.0, 700.0))),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut ev = Vec::new();
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| design.ui(ui, db, &mut ev));
        });
        fn collect(s: &egui::epaint::Shape, out: &mut Vec<(egui::Pos2, String)>) {
            match s {
                egui::epaint::Shape::Text(t) => out.push((t.pos, t.galley.text().to_string())),
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|x| collect(x, out)),
                _ => {}
            }
        }
        let mut texts = Vec::new();
        out.shapes.iter().for_each(|c| collect(&c.shape, &mut texts));
        (texts, ev)
    }

    fn click(at: egui::Pos2) -> Vec<egui::Event> {
        let b = move |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        vec![egui::Event::PointerMoved(at), b(true), b(false)]
    }

    fn settle(ctx: &egui::Context, design: &mut Design, db: &Database, time: &mut f64) -> Vec<(egui::Pos2, String)> {
        let mut texts = Vec::new();
        for _ in 0..3 {
            *time += 0.016;
            texts = frame(ctx, design, db, vec![], *time).0;
        }
        texts
    }

    /// The type drop-down of the Add field row must offer every type, Calculated included.
    #[test]
    fn add_field_dropdown_offers_calculated() {
        let db = album();
        let ctx = egui::Context::default();
        let mut design = Design::new("Album".into());
        let mut time = 1.0;
        let texts = settle(&ctx, &mut design, &db, &mut time);
        // The row's type box shows the current choice; open it.
        let row_y = texts.iter().find(|(_, t)| t == "Add field:").expect("Add field row").0.y;
        let current = texts.iter().find(|(p, t)| t == "TEXT" && (p.y - row_y).abs() < 6.0).expect("type box").0;
        time += 2.0;
        frame(&ctx, &mut design, &db, click(current + egui::vec2(4.0, 4.0)), time);
        let texts = settle(&ctx, &mut design, &db, &mut time);
        let offered: Vec<&str> = texts.iter().map(|(_, t)| t.as_str()).collect();
        for want in ["INTEGER", "BIGNUM", "FLOAT", "DOUBLE", "DECIMAL", "CURRENCY", "RATIONAL", "TEXT", "YESNO", "DATETIME", "CALCULATED"] {
            assert!(offered.contains(&want), "{want} not offered: {offered:?}");
        }
    }

    /// A table that already has a calculated field shows it as one, with its formula.
    #[test]
    fn design_view_shows_existing_calculated_fields() {
        let mut db = album();
        db.execute("ALTER TABLE Album ADD COLUMN Title CALCULATED AS (Name & ' (' & Year & ')')").unwrap();
        let ctx = egui::Context::default();
        let mut design = Design::new("Album".into());
        let mut time = 1.0;
        let texts = settle(&ctx, &mut design, &db, &mut time);
        let shown: Vec<&str> = texts.iter().map(|(_, t)| t.as_str()).collect();
        assert!(shown.contains(&"CALCULATED"), "type not shown: {shown:?}");
        assert!(shown.contains(&"= Name & ' (' & Year & ')'"), "formula not shown: {shown:?}");
        assert!(shown.contains(&"Formula"), "no Formula button: {shown:?}");
    }

    fn press_enter() -> Vec<egui::Event> {
        let key = |pressed| egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Default::default(),
        };
        vec![key(true), key(false)]
    }

    /// Opens the formula box of the `Year` field and types `formula` into it.
    fn start_conversion(
        ctx: &egui::Context,
        design: &mut Design,
        db: &Database,
        time: &mut f64,
        formula: &str,
    ) -> Vec<(egui::Pos2, String)> {
        let texts = settle(ctx, design, db, time);
        let year_y = texts.iter().find(|(_, t)| t == "Year").expect("Year row").0.y;
        let button = texts.iter().find(|(p, t)| t == "Calculate..." && (p.y - year_y).abs() < 8.0).expect("Calculate button").0;
        *time += 2.0;
        frame(ctx, design, db, click(button + egui::vec2(4.0, 4.0)), *time);
        settle(ctx, design, db, time);
        *time += 0.016;
        frame(ctx, design, db, vec![egui::Event::Text(formula.into())], *time);
        settle(ctx, design, db, time)
    }

    #[test]
    fn an_ordinary_field_can_be_made_calculated_after_confirming() {
        let db = album();
        let ctx = egui::Context::default();
        let mut design = Design::new("Album".into());
        let mut time = 1.0;
        // Fields that are keys or AutoNumbers get no such button.
        let texts = settle(&ctx, &mut design, &db, &mut time);
        assert_eq!(texts.iter().filter(|(_, t)| t == "Calculate...").count(), 2, "Name and Year only");

        start_conversion(&ctx, &mut design, &db, &mut time, "Len(Name)");
        time += 0.5;
        frame(&ctx, &mut design, &db, press_enter(), time);
        let texts = settle(&ctx, &mut design, &db, &mut time);
        let prompt = texts.iter().find(|(_, t)| t.starts_with("Replace every value")).expect("a confirmation");
        assert!(prompt.1.contains("Year") && prompt.1.contains("Len(Name)"), "{}", prompt.1);

        // Nothing is changed until Yes.
        let prompt_y = prompt.0.y;
        let yes = texts.iter().find(|(p, t)| t == "Yes" && (p.y - prompt_y).abs() < 8.0).expect("Yes button").0;
        time += 2.0;
        let (_, events) = frame(&ctx, &mut design, &db, click(yes + egui::vec2(4.0, 4.0)), time);
        let sql: Vec<&str> = events.iter().filter_map(|e| if let Event::Execute { sql, .. } = e { Some(sql.as_str()) } else { None }).collect();
        assert_eq!(sql, ["ALTER TABLE [Album] ALTER COLUMN [Year] CALCULATED AS (Len(Name))"]);
    }

    #[test]
    fn a_bad_conversion_formula_is_explained_and_not_offered() {
        let db = album();
        let ctx = egui::Context::default();
        let mut design = Design::new("Album".into());
        let mut time = 1.0;
        start_conversion(&ctx, &mut design, &db, &mut time, "Nope + 1");
        time += 0.5;
        frame(&ctx, &mut design, &db, press_enter(), time);
        let texts = settle(&ctx, &mut design, &db, &mut time);
        assert!(texts.iter().any(|(_, t)| t.contains("not valid") || t.contains("Nope")), "no explanation: {texts:?}");
        assert!(!texts.iter().any(|(_, t)| t.starts_with("Replace every value")), "must not ask to confirm a bad formula");
    }

    /// Opens the type drop-down in the row of field `field` and returns the options it lists.
    fn open_type_box(
        ctx: &egui::Context,
        design: &mut Design,
        db: &Database,
        time: &mut f64,
        field: &str,
        current: &str,
    ) -> Vec<(egui::Pos2, String)> {
        let texts = settle(ctx, design, db, time);
        let row_y = texts.iter().find(|(_, t)| t == field).expect("field row").0.y;
        let boxed = texts.iter().find(|(p, t)| t == current && (p.y - row_y).abs() < 8.0).expect("type box").0;
        *time += 2.0;
        frame(ctx, design, db, click(boxed + egui::vec2(4.0, 4.0)), *time);
        settle(ctx, design, db, time)
    }

    #[test]
    fn existing_fields_offer_calculated_in_their_type_dropdown() {
        let mut db = Database::in_memory();
        db.execute("CREATE TABLE K (code NUMBER PRIMARY KEY, v TEXT, n NUMBER)").unwrap();
        let ctx = egui::Context::default();
        let mut design = Design::new("K".into());
        let mut time = 1.0;

        // An ordinary field: CALCULATED is among the choices, and choosing it asks for a formula.
        let texts = open_type_box(&ctx, &mut design, &db, &mut time, "v", "TEXT");
        let offered: Vec<&str> = texts.iter().map(|(_, t)| t.as_str()).collect();
        assert!(offered.iter().filter(|t| **t == "CALCULATED").count() >= 1, "not offered: {offered:?}");
        let row_y = texts.iter().find(|(_, t)| t == "v").unwrap().0.y;
        let choice = texts.iter().filter(|(_, t)| t == "CALCULATED").map(|(p, _)| *p).find(|p| p.y > row_y).expect("choice");
        time += 2.0;
        frame(&ctx, &mut design, &db, click(choice + egui::vec2(4.0, 4.0)), time);
        let texts = settle(&ctx, &mut design, &db, &mut time);
        assert!(texts.iter().any(|(_, t)| t.starts_with("e.g. First")), "no formula box: {texts:?}");
        // Nothing is changed yet.
        assert_eq!(db.table("K").unwrap().schema().columns[1].data_type, DataType::Text);

        // A key field is not offered it.
        let mut design = Design::new("K".into());
        let texts = open_type_box(&ctx, &mut design, &db, &mut time, "code", "DECIMAL");
        let after_code: Vec<&str> = texts.iter().filter(|(p, _)| p.y > 0.0).map(|(_, t)| t.as_str()).collect();
        let code_y = texts.iter().find(|(_, t)| t == "code").unwrap().0.y;
        let v_y = texts.iter().find(|(_, t)| t == "v").unwrap().0.y;
        let in_popup = texts.iter().filter(|(p, t)| t == "CALCULATED" && p.y > code_y && p.y < v_y).count();
        assert_eq!(in_popup, 0, "a key field must not offer CALCULATED: {after_code:?}");
    }

    #[test]
    fn a_bad_formula_is_explained_and_cannot_be_added() {
        let db = album();
        let ctx = egui::Context::default();
        let mut design = Design::new("Album".into());
        let mut time = 1.0;
        design.add_name = "Title".into();
        design.add_type = DataType::Calculated;
        design.add_formula = "Name && ' (' && Year && ')'".into();
        let texts = settle(&ctx, &mut design, &db, &mut time);

        let message = texts.iter().map(|(_, t)| t.as_str()).find(|t| t.contains("not valid"));
        let message = message.unwrap_or_else(|| panic!("no explanation shown: {texts:?}"));
        assert!(message.contains("Title") && message.contains("single &"), "{message}");

        // Pressing Add does nothing while the formula is wrong.
        let add = texts.iter().find(|(_, t)| t == "Add").expect("Add button").0 + egui::vec2(4.0, 4.0);
        time += 2.0;
        let (_, events) = frame(&ctx, &mut design, &db, click(add), time);
        assert!(events.iter().all(|e| !matches!(e, Event::Execute { .. })), "a bad formula was submitted");
    }

    #[test]
    fn a_good_formula_can_be_added() {
        let db = album();
        let ctx = egui::Context::default();
        let mut design = Design::new("Album".into());
        let mut time = 1.0;
        design.add_name = "Title".into();
        design.add_type = DataType::Calculated;
        design.add_formula = "Name & ' (' & Year & ')'".into();
        let texts = settle(&ctx, &mut design, &db, &mut time);
        assert!(!texts.iter().any(|(_, t)| t.contains("not valid")), "{texts:?}");

        let add = texts.iter().find(|(_, t)| t == "Add").expect("Add button").0 + egui::vec2(4.0, 4.0);
        time += 2.0;
        let (_, events) = frame(&ctx, &mut design, &db, click(add), time);
        let sql: Vec<&str> = events.iter().filter_map(|e| if let Event::Execute { sql, .. } = e { Some(sql.as_str()) } else { None }).collect();
        assert_eq!(sql, ["ALTER TABLE [Album] ADD COLUMN [Title] CALCULATED AS (Name & ' (' & Year & ')')"]);
    }

    /// Every type the engine has (in asql, the default dialect) is offered, an array of each included, and each one
    /// makes a column the engine accepts.
    #[test]
    fn the_type_lists_hold_every_engine_type() {
        for t in TYPES {
            assert!(NEW_TYPES.contains(&t), "{t} is offered for a new field but not for an existing one");
            if t != DataType::Calculated {
                let array = DataType::Array(ArrayOf::of(t));
                assert!(TYPES.contains(&array), "no {array}");
            }
        }
        assert!(TYPES.contains(&DataType::Array(ArrayOf::Any)));
        assert!(NEW_TYPES.contains(&DataType::Calculated));
        let mut db = Database::in_memory();
        for (i, t) in TYPES.iter().enumerate() {
            db.execute(&format!("CREATE TABLE [T{i}] ([A] {})", t.sql_name())).unwrap_or_else(|e| panic!("{t}: {e}"));
            assert_eq!(db.table(&format!("T{i}")).unwrap().schema().columns[0].data_type, *t);
        }
    }
}
