//! Datasheet view: a virtualised, editable grid over one table.

use accessor_core::{DataType, Database, RowId, Value};
use egui::{Id, Key, Label, RichText, Sense, TextEdit};
use egui_extras::{Column, TableBuilder};

use crate::lookup::{self, Lookup};
use crate::ui::Event;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Row(RowId),
    /// The blank "new record" row at the bottom.
    New,
}

struct Edit {
    target: Target,
    col: usize,
    buf: String,
    fresh: bool,
}

type Pending = Option<(Target, usize, Value)>;

pub struct Datasheet {
    pub table: String,
    ids: Vec<RowId>,
    dirty: bool,
    filter: String,
    applied_filter: String,
    sort: Option<(usize, bool)>,
    selected: Option<RowId>,
    edit: Option<Edit>,
    /// Drop-down data for fields that refer to another table.
    lookups: Vec<Option<Lookup>>,
    /// Scroll to the blank "new record" row on the next frame.
    scroll_new: bool,
    /// Values typed into the blank row so far; saved once every required field is filled.
    draft: Vec<Option<Value>>,
}

impl Datasheet {
    pub fn new(table: String) -> Self {
        Datasheet {
            table,
            ids: Vec::new(),
            dirty: true,
            filter: String::new(),
            applied_filter: String::new(),
            sort: None,
            selected: None,
            edit: None,
            lookups: Vec::new(),
            scroll_new: false,
            draft: Vec::new(),
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.edit = None;
    }

    fn rebuild(&mut self, db: &Database) {
        self.ids.clear();
        self.lookups = lookup::build(db, &self.table);
        let Some(t) = db.table(&self.table) else { return };
        let needle = self.filter.to_lowercase();
        self.ids.extend(
            t.iter()
                .filter(|(_, r)| needle.is_empty() || r.iter().any(|v| v.to_string().to_lowercase().contains(&needle)))
                .map(|(id, _)| id),
        );
        if let Some((c, desc)) = self.sort {
            self.ids.sort_by(|a, b| {
                let (ra, rb) = (t.get(*a).expect("live id"), t.get(*b).expect("live id"));
                ra[c].cmp(&rb[c])
            });
            if desc {
                self.ids.reverse();
            }
        }
        if self.selected.is_some_and(|s| t.get(s).is_none()) {
            self.selected = None;
        }
    }

    /// Each view draws under its own widget-id namespace. Views share one panel, so
    /// without this their automatic ids collide and egui carries click and focus
    /// state from one view into the next.
    pub fn ui(&mut self, ui: &mut egui::Ui, db: &mut Database, ev: &mut Vec<Event>) {
        let id = ("datasheet", self.table.clone());
        ui.push_id(id, |ui| self.show(ui, db, ev));
    }

    fn show(&mut self, ui: &mut egui::Ui, db: &mut Database, ev: &mut Vec<Event>) {
        if self.dirty || self.filter != self.applied_filter {
            self.applied_filter = self.filter.clone();
            self.rebuild(db);
            self.dirty = false;
        }
        let Some(t) = db.table(&self.table) else {
            ui.label("This table no longer exists.");
            return;
        };
        let columns = t.schema().columns.clone();
        if self.draft.len() != columns.len() {
            self.draft = vec![None; columns.len()];
        }
        let required: Vec<bool> =
            columns.iter().map(|c| (c.not_null || c.primary_key) && !c.autonumber && c.default.is_none()).collect();

        let mut delete = false;
        ui.horizontal(|ui| {
            if ui.button("New record").clicked() {
                // Start typing in the first field the user can fill in.
                let col = columns.iter().position(|c| !c.autonumber && !c.is_calculated()).unwrap_or(0);
                self.edit = Some(Edit { target: Target::New, col, buf: String::new(), fresh: true });
                self.scroll_new = true;
                ui.ctx().request_repaint();
            }
            ui.label("Search:");
            ui.add(TextEdit::singleline(&mut self.filter).desired_width(220.0).hint_text("any field"));
            if ui.add_enabled(self.selected.is_some(), egui::Button::new("Delete record")).clicked() {
                delete = true;
            }
            if self.draft.iter().any(Option::is_some) && ui.button("Discard new record").clicked() {
                self.draft.fill(None);
            }
            ui.label(RichText::new(format!("{} of {} records", self.ids.len(), t.len())).weak());
        });
        ui.separator();

        let mut pending: Pending = None;
        let mut sort_click: Option<usize> = None;
        let mut select: Option<RowId> = None;
        let (ids, sort, edit, selected, lookups, draft) =
            (&self.ids, self.sort, &mut self.edit, self.selected, &self.lookups, &self.draft);
        let table_name = self.table.clone();

        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .column(Column::exact(40.0))
            .columns(Column::initial(130.0).at_least(50.0).clip(true), columns.len());
        if std::mem::take(&mut self.scroll_new) {
            builder = builder.scroll_to_row(ids.len(), Some(egui::Align::BOTTOM));
        }
        builder
            .header(24.0, |mut h| {
                h.col(|_| {});
                for (c, col) in columns.iter().enumerate() {
                    h.col(|ui| {
                        let arrow = match sort {
                            Some((sc, desc)) if sc == c => {
                                if desc {
                                    " v"
                                } else {
                                    " ^"
                                }
                            }
                            _ => "",
                        };
                        let r = ui.add(Label::new(RichText::new(format!("{}{}{arrow}", col.name, if required[c] { " *" } else { "" })).strong()).sense(Sense::click()));
                        let mut tip = col.data_type.to_string();
                        if col.primary_key {
                            tip.push_str(", primary key");
                        }
                        if col.autonumber {
                            tip.push_str(", AutoNumber");
                        }
                        if let Some(expr) = col.expression() {
                            tip = format!("calculated: = {expr} (read-only)");
                        }
                        if let Some(l) = lookups.get(c).and_then(Option::as_ref) {
                            tip.push_str(&format!(", refers to {}", l.parent));
                        }
                        if r.on_hover_text(tip).clicked() {
                            sort_click = Some(c);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(22.0, ids.len() + 1, |mut row| {
                    let i = row.index();
                    let target = ids.get(i).map_or(Target::New, |&id| Target::Row(id));
                    let record = match target {
                        Target::Row(id) => t.get(id),
                        Target::New => None,
                    };
                    row.set_selected(matches!(target, Target::Row(id) if Some(id) == selected));
                    row.col(|ui| {
                        let label = if target == Target::New { "*".to_string() } else { (i + 1).to_string() };
                        ui.label(RichText::new(label).weak());
                    });
                    for (c, col) in columns.iter().enumerate() {
                        row.col(|ui| {
                            let id = Id::new(("cell", &table_name, i, c));
                            let value = match target {
                                Target::Row(_) => record.map(|r| &r[c]),
                                Target::New => draft[c].as_ref(),
                            };
                            let lk = lookups.get(c).and_then(Option::as_ref);
                            if cell(ui, id, target, c, col.data_type, value, lk, edit, &mut pending) {
                                if let Target::Row(rid) = target {
                                    select = Some(rid);
                                }
                            }
                        });
                    }
                });
            });

        if sort_click.is_some() || select.is_some() || delete || pending.is_some() {
            ui.ctx().request_repaint();
        }
        if select.is_some() {
            self.selected = select;
        }
        if let Some(c) = sort_click {
            self.sort = match self.sort {
                Some((sc, false)) if sc == c => Some((c, true)),
                Some((sc, true)) if sc == c => None,
                _ => Some((c, false)),
            };
            self.dirty = true;
        }
        if delete {
            if let Some(id) = self.selected {
                match db.delete_row(&self.table, id) {
                    Ok(()) => {
                        self.selected = None;
                        ev.push(Event::Changed);
                    }
                    Err(e) => ev.push(Event::Error(e.to_string())),
                }
            }
        }
        if let Some((target, col, value)) = pending {
            match target {
                Target::Row(id) => match db.update_cell(&self.table, id, col, value) {
                    Ok(()) => ev.push(Event::Changed),
                    Err(e) => ev.push(Event::Error(e.to_string())),
                },
                Target::New => {
                    self.draft[col] = (!value.is_null()).then_some(value);
                    let missing: Vec<&str> = (0..columns.len())
                        .filter(|&c| required[c] && self.draft[c].is_none())
                        .map(|c| columns[c].name.as_str())
                        .collect();
                    if missing.is_empty() {
                        let values: Vec<(usize, Value)> = self
                            .draft
                            .iter()
                            .enumerate()
                            .filter_map(|(c, v)| v.clone().map(|v| (c, v)))
                            .collect();
                        match db.insert_row(&self.table, &values) {
                            Ok(_) => {
                                self.draft.fill(None);
                                ev.push(Event::Changed);
                            }
                            // Keep the typed values so they can be corrected.
                            Err(e) => ev.push(Event::Error(e.to_string())),
                        }
                    } else if self.draft.iter().any(Option::is_some) {
                        ev.push(Event::Status(format!("New record not saved yet - fill in: {}", missing.join(", "))));
                    }
                }
            }
        }
    }
}

/// Draws one cell; returns true if it was clicked.
#[allow(clippy::too_many_arguments)]
fn cell(
    ui: &mut egui::Ui,
    id: Id,
    target: Target,
    col: usize,
    ty: DataType,
    value: Option<&Value>,
    lookup: Option<&Lookup>,
    edit: &mut Option<Edit>,
    pending: &mut Pending,
) -> bool {
    // A selected row draws its text in the selection colour (white on red). Inputs keep
    // their own light background, so they need the normal text colour back.
    let input_cell = lookup.is_some() || ty == DataType::YesNo || edit.as_ref().is_some_and(|e| e.target == target && e.col == col);
    if input_cell {
        ui.visuals_mut().override_text_color = None;
    }
    if edit.as_ref().is_some_and(|e| e.target == target && e.col == col) {
        let e = edit.as_mut().expect("checked");
        // Take focus before the box exists so the very next key press lands in it.
        if e.fresh {
            ui.memory_mut(|m| m.request_focus(id));
            e.fresh = false;
        }
        let resp = ui.add(TextEdit::singleline(&mut e.buf).id(id).desired_width(f32::INFINITY));
        if resp.lost_focus() {
            ui.ctx().request_repaint();
            let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
            let done = edit.take().expect("checked");
            if !cancelled {
                let v = if done.buf.is_empty() { Value::Null } else { Value::text(&done.buf) };
                *pending = Some((done.target, done.col, v));
            }
        }
        return false;
    }
    if let Some(lk) = lookup {
        if let Some(v) = lookup::combo(ui, id, lk, value.unwrap_or(&Value::Null), ui.available_width()) {
            *pending = Some((target, col, v));
        }
        return false;
    }
    if ty == DataType::YesNo {
        let mut b = matches!(value, Some(Value::Bool(true)));
        if ui.checkbox(&mut b, "").changed() {
            *pending = Some((target, col, Value::Bool(b)));
        }
        return false;
    }
    let text = value.map(ToString::to_string).unwrap_or_default();
    let rect = ui.max_rect();
    ui.add(Label::new(&text).truncate());
    let resp = ui.interact(rect, id, Sense::click());
    // A calculated value belongs to its formula, so it cannot be edited here.
    if resp.double_clicked() && ty != DataType::Calculated {
        *edit = Some(Edit { target, col, buf: text, fresh: true });
        // The box is drawn by the next frame, which nothing else would trigger.
        ui.ctx().request_repaint();
    }
    resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Session {
        ctx: egui::Context,
        sheet: Datasheet,
        db: Database,
        time: f64,
        texts: Vec<(egui::Pos2, String)>,
        clips: Vec<(String, egui::Rect)>,
        colors: Vec<(String, egui::Color32)>,
        events: Vec<Event>,
    }

    impl Session {
        fn new(db: Database) -> Self {
            Session { ctx: egui::Context::default(), sheet: Datasheet::new("T".into()), db, time: 1.0, texts: vec![], clips: vec![], colors: vec![], events: vec![] }
        }

        /// One input event batch, then however many extra frames the app asked for,
        /// as a real window would: it does not redraw unless asked.
        fn step(&mut self, events: Vec<egui::Event>) {
            let mut more = self.frame(events);
            for _ in 0..5 {
                if !more {
                    break;
                }
                more = self.frame(vec![]);
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> bool {
            self.time += 0.016;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (sheet, db) = (&mut self.sheet, &mut self.db);
            let mut ev = Vec::new();
            let out = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| sheet.ui(ui, db, &mut ev));
            });
            for e in &ev {
                if matches!(e, Event::Changed) {
                    self.sheet.mark_dirty();
                }
            }
            self.events.extend(ev);
            fn collect(s: &egui::epaint::Shape, out: &mut Vec<(egui::Pos2, String)>) {
                match s {
                    egui::epaint::Shape::Text(t) => out.push((t.pos, t.galley.text().to_string())),
                    egui::epaint::Shape::Vec(v) => v.iter().for_each(|x| collect(x, out)),
                    _ => {}
                }
            }
            self.texts.clear();
            out.shapes.iter().for_each(|c| collect(&c.shape, &mut self.texts));
            self.clips = out
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::epaint::Shape::Text(t) => Some((t.galley.text().to_string(), c.clip_rect)),
                    _ => None,
                })
                .collect();
            self.colors = out
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::epaint::Shape::Text(t) => {
                        let section = t.galley.job.sections.first().map(|s| s.format.color);
                        let color = t.override_text_color.or(section).filter(|c| *c != egui::Color32::PLACEHOLDER);
                        Some((t.galley.text().to_string(), color.unwrap_or(t.fallback_color)))
                    }
                    _ => None,
                })
                .collect();
            out.viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.repaint_delay.is_zero())
        }

        fn settle(&mut self) {
            for _ in 0..3 {
                self.step(vec![]);
            }
        }

        fn pos_of(&self, label: &str) -> egui::Pos2 {
            self.texts.iter().find(|(_, t)| t == label).unwrap_or_else(|| panic!("{label:?} not drawn: {:?}", self.texts)).0
        }

        fn double_click(&mut self, at: egui::Pos2) {
            let b = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            self.step(vec![egui::Event::PointerMoved(at)]);
            for _ in 0..2 {
                self.step(vec![b(true)]);
                self.step(vec![b(false)]);
            }
            // no extra frames: a real window only redraws on input or request
        }

        fn type_text(&mut self, text: &str) {
            self.step(vec![egui::Event::Text(text.to_string())]);
            // no extra frames: a real window only redraws on input or request
        }

        fn drawn(&self, text: &str) -> bool {
            self.texts.iter().any(|(_, t)| t == text)
        }
    }

    #[test]
    fn editing_in_a_table_with_a_relationship_after_adding_a_field() {
        let mut db = Database::in_memory();
        db.execute(
            "CREATE TABLE Artist (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL);
             CREATE TABLE Album (ID COUNTER PRIMARY KEY, Name TEXT NOT NULL, Artist_ID INTEGER NOT NULL REFERENCES Artist (ID));
             INSERT INTO Artist (Name) VALUES ('Grand Funk'), ('Yes');
             INSERT INTO Album (Name, Artist_ID) VALUES ('On Time', 1), ('Fragile', 2)",
        )
        .unwrap();
        let mut s = Session::new(db);
        s.sheet = Datasheet::new("Album".into());
        s.settle();
        s.db.execute("ALTER TABLE Album ADD COLUMN Year INTEGER").unwrap();
        s.sheet.mark_dirty();
        s.settle();
        s.time += 2.0;
        let at = s.pos_of("On Time") + egui::vec2(4.0, 4.0);
        s.double_click(at);
        s.type_text("Z");
        assert!(visible(&s, "On TimeZ"), "existing field: {:?}", s.clips);
        s.step(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
        s.settle();
        assert_eq!(s.db.query("SELECT Name FROM Album WHERE ID = 1").unwrap().rows[0][0].to_string(), "On TimeZ");
        // The new field.
        s.time += 2.0;
        let year = s.pos_of("Year");
        s.double_click(egui::pos2(year.x + 4.0, s.pos_of("Fragile").y + 4.0));
        s.type_text("1971");
        assert!(visible(&s, "1971"), "new field: {:?}", s.clips);
    }

    /// The app's real colour scheme: light, with white text on a red selection.
    fn app_visuals() -> egui::Visuals {
        let mut v = egui::Visuals::light();
        v.selection.bg_fill = egui::Color32::from_rgb(164, 55, 58);
        v.selection.stroke.color = egui::Color32::WHITE;
        v
    }

    #[test]
    fn edited_text_is_readable_in_a_selected_row() {
        let mut s = Session::new(table());
        s.ctx.set_visuals(app_visuals());
        s.settle();
        s.time += 2.0;
        let at = s.pos_of("alpha") + egui::vec2(4.0, 4.0);
        // Select the row first, as the first click of a double-click does.
        s.double_click(at);
        s.type_text("Z");
        let color = s.colors.iter().find(|(t, _)| t == "alphaZ").map(|(_, c)| *c).expect("edit box drawn");
        // The edit box is white in this scheme; white text on it cannot be read.
        assert_ne!(color, egui::Color32::WHITE, "edited text is drawn white on a white box");
    }

    #[test]
    fn calculated_cells_are_read_only_and_follow_edits() {
        let mut db = Database::in_memory();
        db.execute(
            "CREATE TABLE T (ID COUNTER PRIMARY KEY, Qty INTEGER, Price INTEGER, Total CALCULATED AS (Qty * Price));
             INSERT INTO T (Qty, Price) VALUES (3, 4)",
        )
        .unwrap();
        let mut s = Session::new(db);
        s.settle();
        s.time += 2.0;
        assert!(s.drawn("12"), "computed value shown: {:?}", s.texts);

        // The calculated cell does not go into edit mode.
        let total = s.pos_of("12") + egui::vec2(4.0, 4.0);
        s.double_click(total);
        assert!(s.sheet.edit.is_none(), "a calculated cell must not be editable");

        // Editing a source field recomputes it.
        s.time += 2.0;
        let (qty_x, row_y) = (s.pos_of("Qty").x, s.pos_of("12").y);
        s.double_click(egui::pos2(qty_x + 4.0, row_y + 4.0));
        s.type_text("0");
        let enter = |pressed| egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Default::default(),
        };
        s.step(vec![enter(true), enter(false)]);
        s.settle();
        assert!(s.drawn("120"), "total follows the edit: {:?}", s.texts);
        assert_eq!(s.db.query("SELECT Total FROM T").unwrap().rows[0][0].to_string(), "120");
    }

    fn visible(s: &Session, text: &str) -> bool {
        s.clips.iter().any(|(t, clip)| t == text && clip.width() > 4.0 && clip.height() > 4.0)
    }

    fn table() -> Database {
        let mut db = Database::in_memory();
        db.execute("CREATE TABLE T (ID COUNTER PRIMARY KEY, Name TEXT); INSERT INTO T (Name) VALUES ('alpha'), ('beta')").unwrap();
        db
    }

    #[test]
    fn editing_a_cell_shows_typed_text() {
        let mut s = Session::new(table());
        s.settle();
        let at = s.pos_of("alpha") + egui::vec2(4.0, 4.0);
        s.double_click(at);
        s.type_text("Z");
        assert!(s.drawn("alphaZ") || s.drawn("Z"), "typed text not shown: {:?}", s.texts);
    }

    /// The real route: add the field in Design view with clicks and typing,
    /// then open the datasheet in the same window.
    #[test]
    fn editing_after_adding_a_field_in_design_view() {
        use crate::design::Design;

        let mut s = Session::new(table());
        let mut design = Design::new("T".into());
        let mut run_design = |s: &mut Session, design: &mut Design, events: Vec<egui::Event>| {
            s.time += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0))),
                time: Some(s.time),
                events,
                ..Default::default()
            };
            let mut ev = Vec::new();
            let (db, ctx) = (&s.db, s.ctx.clone());
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| design.ui(ui, db, &mut ev));
            });
            for e in ev {
                if let Event::Execute { sql, .. } = e {
                    s.db.execute(&sql).unwrap();
                }
            }
            fn collect(sh: &egui::epaint::Shape, out: &mut Vec<(egui::Pos2, String)>) {
                match sh {
                    egui::epaint::Shape::Text(t) => out.push((t.pos, t.galley.text().to_string())),
                    egui::epaint::Shape::Vec(v) => v.iter().for_each(|x| collect(x, out)),
                    _ => {}
                }
            }
            s.texts.clear();
            out.shapes.iter().for_each(|c| collect(&c.shape, &mut s.texts));
        };
        let click = |at: egui::Pos2| {
            let b = move |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            vec![egui::Event::PointerMoved(at), b(true), b(false)]
        };

        for _ in 0..3 {
            run_design(&mut s, &mut design, vec![]);
        }
        let name_box = s.pos_of("name") + egui::vec2(4.0, 4.0);
        run_design(&mut s, &mut design, click(name_box));
        run_design(&mut s, &mut design, vec![egui::Event::Text("Notes".into())]);
        for _ in 0..3 {
            run_design(&mut s, &mut design, vec![]);
        }
        let add = s.pos_of("Add") + egui::vec2(4.0, 4.0);
        run_design(&mut s, &mut design, click(add));
        for _ in 0..3 {
            run_design(&mut s, &mut design, vec![]);
        }
        assert!(s.db.table("T").unwrap().schema().column_index("Notes").is_some(), "field was not added");

        // Now the datasheet, as if the user switched tabs.
        s.time += 3.0; // a person takes a moment to switch tabs
        s.sheet.mark_dirty();
        s.settle();
        let at = s.pos_of("alpha") + egui::vec2(4.0, 4.0);
        s.double_click(at);
        s.type_text("Z");
        assert!(visible(&s, "alphaZ"), "not visible: {:?}", s.clips);

        // The new, still empty field.
        s.step(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
        s.settle();
        s.time += 2.0;
        let notes = egui::pos2(332.0 + 6.0, 65.4 + 6.0);
        s.double_click(notes);
        s.type_text("Q");
        assert!(visible(&s, "Q"), "new field not visible: {:?}", s.clips);
    }
}
