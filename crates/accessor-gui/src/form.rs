//! Form view: one record at a time with navigation.

use accessor_core::{DataType, Database, RowId, Value};
use egui::{RichText, TextEdit};

use crate::lookup::{self, Lookup};
use crate::ui::Event;

pub struct Form {
    pub table: String,
    ids: Vec<RowId>,
    pos: usize,
    bufs: Vec<String>,
    orig: Vec<String>,
    loaded: Option<RowId>,
    adding: bool,
    dirty: bool,
    lookups: Vec<Option<Lookup>>,
}

impl Form {
    pub fn new(table: String) -> Self {
        Form { table, ids: Vec::new(), pos: 0, bufs: Vec::new(), orig: Vec::new(), loaded: None, adding: false, dirty: true, lookups: Vec::new() }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.loaded = None;
    }

    /// Brings the cached record list and field buffers in line with the database.
    /// Returns the table's fields and the record being shown (`None` when adding).
    fn sync(&mut self, db: &Database) -> Option<(Vec<accessor_core::Column>, Option<RowId>)> {
        let t = db.table(&self.table)?;
        if self.dirty {
            self.lookups = lookup::build(db, &self.table);
            self.ids = t.iter().map(|(id, _)| id).collect();
            self.dirty = false;
        }
        self.pos = self.pos.min(self.ids.len().saturating_sub(1));
        // An empty table has nothing to show, so go straight to a blank new record;
        // otherwise the fields would look editable but could never be saved.
        if self.ids.is_empty() {
            self.adding = true;
        }
        let columns = t.schema().columns.clone();
        let current = if self.adding { None } else { self.ids.get(self.pos).copied() };

        if self.loaded != current || self.bufs.len() != columns.len() {
            let row = current.and_then(|id| t.get(id));
            self.orig = (0..columns.len()).map(|c| row.map_or(String::new(), |r| r[c].to_string())).collect();
            self.bufs = self.orig.clone();
            self.loaded = current;
        }
        Some((columns, current))
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, db: &mut Database, ev: &mut Vec<Event>) {
        let id = ("form", self.table.clone());
        ui.push_id(id, |ui| self.show(ui, db, ev));
    }

    fn show(&mut self, ui: &mut egui::Ui, db: &mut Database, ev: &mut Vec<Event>) {
        let Some((columns, current)) = self.sync(db) else {
            ui.label("This table no longer exists.");
            return;
        };

        let total = self.ids.len();
        let mut go: Option<usize> = None;
        let mut start_add = false;
        let mut delete = false;
        ui.horizontal(|ui| {
            let can_back = self.pos > 0 && !self.adding;
            let can_fwd = self.pos + 1 < total && !self.adding;
            if ui.add_enabled(can_back, egui::Button::new("|<")).clicked() {
                go = Some(0);
            }
            if ui.add_enabled(can_back, egui::Button::new("<")).clicked() {
                go = Some(self.pos - 1);
            }
            let label = if self.adding {
                "New record".to_string()
            } else if total == 0 {
                "No records".to_string()
            } else {
                format!("Record {} of {total}", self.pos + 1)
            };
            ui.label(RichText::new(label).strong());
            if ui.add_enabled(can_fwd, egui::Button::new(">")).clicked() {
                go = Some(self.pos + 1);
            }
            if ui.add_enabled(can_fwd, egui::Button::new(">|")).clicked() {
                go = Some(total - 1);
            }
            ui.separator();
            if ui.add_enabled(!self.adding, egui::Button::new("New")).clicked() {
                start_add = true;
            }
            if ui.add_enabled(current.is_some(), egui::Button::new("Delete")).clicked() {
                delete = true;
            }
        });
        ui.separator();

        let mut commit_field: Option<usize> = None;
        let mut add_now = false;
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new(("form", &self.table)).num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                for (c, col) in columns.iter().enumerate() {
                    let required = (col.not_null || col.primary_key) && !col.autonumber && col.default.is_none();
                    ui.label(RichText::new(if required { format!("{} *", col.name) } else { col.name.clone() }).strong());
                    if let Some(lk) = self.lookups.get(c).and_then(Option::as_ref) {
                        let current = match self.bufs[c].as_str() {
                            "" => Value::Null,
                            s => lk.options.iter().map(|(k, _)| k).find(|k| k.to_string() == s).cloned().unwrap_or(Value::Null),
                        };
                        if let Some(v) = lookup::combo(ui, ("form_lookup", &self.table, c), lk, &current, 360.0) {
                            self.bufs[c] = if v.is_null() { String::new() } else { v.to_string() };
                            if !self.adding {
                                commit_field = Some(c);
                            }
                        }
                    } else if col.is_calculated() {
                        // Computed from the other fields when the record is saved.
                        let shown = if self.bufs[c].is_empty() { "(calculated when saved)" } else { self.bufs[c].as_str() };
                        ui.label(RichText::new(shown).weak());
                    } else if col.data_type == DataType::YesNo {
                        let mut b = matches!(self.bufs[c].as_str(), "Yes");
                        if ui.checkbox(&mut b, "").changed() {
                            self.bufs[c] = if b { "Yes" } else { "No" }.to_string();
                            commit_field = Some(c);
                        }
                    } else {
                        let r = ui.add(TextEdit::singleline(&mut self.bufs[c]).desired_width(360.0));
                        if r.lost_focus() && self.bufs[c] != self.orig[c] && !self.adding {
                            commit_field = Some(c);
                        }
                    }
                    ui.end_row();
                }
            });
            if self.adding {
                ui.label(RichText::new("* required").weak());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Add record").clicked() {
                        add_now = true;
                    }
                    if !self.ids.is_empty() && ui.button("Cancel").clicked() {
                        self.adding = false;
                        self.loaded = None;
                        ui.ctx().request_repaint();
                    }
                });
            }
        });

        // State changes below land after the widgets were drawn, so ask for a
        // second frame; otherwise the old record stays on screen until the next input.
        if go.is_some() || start_add || delete || commit_field.is_some() || add_now {
            ui.ctx().request_repaint();
        }
        if let Some(p) = go {
            self.pos = p;
        }
        if start_add {
            self.begin_add(columns.len());
        }
        if delete {
            if let Some(id) = current {
                match db.delete_row(&self.table, id) {
                    Ok(()) => ev.push(Event::Changed),
                    Err(e) => ev.push(Event::Error(e.to_string())),
                }
            }
        }
        if let (Some(c), Some(id)) = (commit_field, current) {
            let v = if self.bufs[c].is_empty() { Value::Null } else { Value::text(&self.bufs[c]) };
            match db.update_cell(&self.table, id, c, v) {
                Ok(()) => ev.push(Event::Changed),
                Err(e) => {
                    self.loaded = None; // reload the stored values
                    ev.push(Event::Error(e.to_string()));
                }
            }
        }
        if add_now {
            self.add_record(db, &columns, ev);
        }
    }

    /// Starts a blank record.
    fn begin_add(&mut self, fields: usize) {
        self.adding = true;
        self.loaded = None;
        self.bufs = vec![String::new(); fields];
        self.orig = self.bufs.clone();
    }

    /// Saves the buffers as a new record if the required fields are filled in.
    fn add_record(&mut self, db: &mut Database, columns: &[accessor_core::Column], ev: &mut Vec<Event>) {
        let missing: Vec<&str> = columns
            .iter()
            .zip(&self.bufs)
            .filter(|(c, b)| b.is_empty() && (c.not_null || c.primary_key) && !c.autonumber && c.default.is_none())
            .map(|(c, _)| c.name.as_str())
            .collect();
        if !missing.is_empty() {
            ev.push(Event::Error(format!("Fill in the required fields first: {}", missing.join(", "))));
            return;
        }
        let vals: Vec<(usize, Value)> = self
            .bufs
            .iter()
            .enumerate()
            .filter(|(c, b)| !b.is_empty() && !columns[*c].is_calculated())
            .map(|(c, b)| (c, Value::text(b)))
            .collect();
        match db.insert_row(&self.table, &vals) {
            Ok(_) => {
                self.adding = false;
                self.loaded = None;
                self.dirty = true;
                self.pos = usize::MAX; // jump to the new (last) record
                ev.push(Event::Changed);
            }
            Err(e) => ev.push(Event::Error(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn music() -> Database {
        let mut db = Database::in_memory();
        db.execute(
            "CREATE TABLE Artist (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL);
             CREATE TABLE Album (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL, Artist_ID INTEGER NOT NULL REFERENCES Artist (ID));
             INSERT INTO Artist (Name) VALUES ('Grand Funk'), ('Yes'), ('Rush');",
        )
        .unwrap();
        db
    }

    /// Types `values` into the blank record and presses Add record.
    fn add(form: &mut Form, db: &mut Database, values: &[&str]) {
        let (columns, _) = form.sync(db).unwrap();
        if !form.adding {
            form.begin_add(columns.len());
            form.sync(db);
        }
        for (b, v) in form.bufs.iter_mut().zip(values) {
            *b = v.to_string();
        }
        let mut ev = Vec::new();
        form.add_record(db, &columns, &mut ev);
        assert!(!ev.iter().any(|e| matches!(e, Event::Error(_))), "add failed");
        form.sync(db);
    }

    /// Runs one frame of the form and returns every piece of text it drew,
    /// with its position.
    fn frame(ctx: &egui::Context, form: &mut Form, db: &mut Database, events: Vec<egui::Event>) -> Vec<(egui::Pos2, String)> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0))),
            events,
            ..Default::default()
        };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut ev = Vec::new();
                form.ui(ui, db, &mut ev);
            });
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
        texts
    }

    fn click(pos: egui::Pos2) -> Vec<egui::Event> {
        let b = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        vec![egui::Event::PointerMoved(pos), b(true), b(false)]
    }

    /// Plays the form like a user: real clicks and typing, frame by frame.
    struct Session {
        ctx: egui::Context,
        form: Form,
        db: Database,
        texts: Vec<(egui::Pos2, String)>,
    }

    impl Session {
        fn step(&mut self, events: Vec<egui::Event>) {
            self.texts = frame(&self.ctx, &mut self.form, &mut self.db, events);
        }
        fn settle(&mut self) {
            for _ in 0..3 {
                self.step(vec![]);
            }
        }
        fn pos_of(&self, label: &str) -> egui::Pos2 {
            self.texts
                .iter()
                .find(|(_, t)| t == label)
                .unwrap_or_else(|| panic!("{label:?} not on screen: {:?}", self.texts))
                .0
        }
        fn click_label(&mut self, label: &str) {
            let p = self.pos_of(label) + egui::vec2(6.0, 6.0);
            self.step(click(p));
            self.settle();
        }
        /// Clicks in the box to the right of `label` and types.
        fn type_into(&mut self, label: &str, text: &str) {
            let p = self.pos_of(label) + egui::vec2(160.0, 6.0);
            self.step(click(p));
            self.step(vec![egui::Event::Text(text.to_string())]);
            self.settle();
        }
        fn shown(&self) -> Vec<String> {
            self.texts.iter().map(|(_, t)| t.clone()).collect()
        }
    }

    #[test]
    fn three_albums_added_through_the_buttons() {
        let mut s = Session { ctx: egui::Context::default(), form: Form::new("Album".into()), db: music(), texts: vec![] };
        s.settle();
        for (name, artist) in [("On Time", "Grand Funk"), ("Close to the Edge", "Yes"), ("Moving Pictures", "Rush")] {
            if s.shown().iter().any(|t| t == "New") && !s.shown().iter().any(|t| t == "Add record") {
                s.click_label("New");
            }
            s.type_into("Name *", name);
            // Open the Artist_ID drop-down and pick an artist.
            let p = s.pos_of("Artist_ID *") + egui::vec2(160.0, 6.0);
            s.step(click(p));
            s.settle();
            s.click_label(artist);
            s.click_label("Add record");
        }
        let rows = s.db.query("SELECT ID, Name, Artist_ID FROM Album ORDER BY ID").unwrap().rows;
        let stored: Vec<String> = rows.iter().map(|r| format!("{}|{}|{}", r[0], r[1], r[2])).collect();
        assert_eq!(stored, ["1|On Time|1", "2|Close to the Edge|2", "3|Moving Pictures|3"]);

        // Step back through them with the real arrow buttons.
        for want in ["Moving Pictures", "Close to the Edge", "On Time"] {
            assert!(s.shown().iter().any(|t| t == want), "expected {want}: {:?}", s.shown());
            if want != "On Time" {
                s.click_label("<");
            }
        }
    }
}
