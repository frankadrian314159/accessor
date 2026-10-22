//! Application shell: menu, navigation pane, tabs, dialogs.

use std::path::{Path, PathBuf};

use accessor_core::{csv, Database};
use egui::{Color32, RichText};

use crate::datasheet::Datasheet;
use crate::design::{Design, NewTable};
use crate::form::Form;
use crate::query::QueryTab;
use crate::sample::SAMPLE_SQL;
use crate::ui::{bracket, Event};

const ACCENT: Color32 = Color32::from_rgb(164, 55, 58);

enum Tab {
    Datasheet(Datasheet),
    Form(Form),
    Design(Design),
    Query(QueryTab),
}

impl Tab {
    fn title(&self) -> String {
        match self {
            Tab::Datasheet(d) => d.table.clone(),
            Tab::Form(f) => format!("{} (Form)", f.table),
            Tab::Design(d) => format!("{} (Design)", d.table),
            Tab::Query(q) => q.name.clone().unwrap_or_else(|| "Query".to_string()),
        }
    }

    /// Identity used to re-focus an already open view instead of duplicating it.
    fn key(&self) -> Option<String> {
        match self {
            Tab::Datasheet(d) => Some(format!("ds:{}", d.table.to_lowercase())),
            Tab::Form(f) => Some(format!("form:{}", f.table.to_lowercase())),
            Tab::Design(d) => Some(format!("design:{}", d.table.to_lowercase())),
            Tab::Query(q) => q.name.as_ref().map(|n| format!("q:{}", n.to_lowercase())),
        }
    }

    fn references(&self, object: &str) -> bool {
        let same = |s: &str| s.eq_ignore_ascii_case(object);
        match self {
            Tab::Datasheet(d) => same(&d.table),
            Tab::Form(f) => same(&f.table),
            Tab::Design(d) => same(&d.table),
            Tab::Query(q) => q.name.as_deref().is_some_and(same),
        }
    }

    fn mark_dirty(&mut self) {
        match self {
            Tab::Datasheet(d) => d.mark_dirty(),
            Tab::Form(f) => f.mark_dirty(),
            Tab::Design(_) | Tab::Query(_) => {}
        }
    }
}

enum Confirm {
    DropTable(String),
    DropQuery(String),
    DropTrigger(String),
}

pub struct App {
    db: Database,
    path: Option<PathBuf>,
    tabs: Vec<Tab>,
    active: usize,
    status: String,
    error: Option<String>,
    new_table: Option<NewTable>,
    confirm: Option<Confirm>,
    about: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<String>) -> Self {
        let mut visuals = egui::Visuals::light();
        visuals.selection.bg_fill = ACCENT;
        visuals.selection.stroke.color = Color32::WHITE;
        visuals.hyperlink_color = ACCENT;
        cc.egui_ctx.set_visuals(visuals);

        let mut app = App {
            db: Database::in_memory(),
            path: None,
            tabs: Vec::new(),
            active: 0,
            status: "Ready. Create or open a database from the File menu — or load the sample data.".into(),
            error: None,
            new_table: None,
            confirm: None,
            about: false,
        };
        if let Some(p) = path {
            app.open_db(PathBuf::from(p));
        }
        app
    }

    fn open_db(&mut self, path: PathBuf) {
        self.tabs.clear();
        self.active = 0;
        self.db = Database::in_memory(); // release the previous file first
        match Database::open(&path) {
            Ok(db) => {
                self.status = format!("Opened {}", path.display());
                self.db = db;
                self.path = Some(path);
                self.error = None;
            }
            Err(e) => {
                self.path = None;
                self.error = Some(format!("Could not open '{}': {e}", path.display()));
            }
        }
    }

    fn open_tab(&mut self, tab: Tab) {
        if let Some(k) = tab.key() {
            if let Some(i) = self.tabs.iter().position(|t| t.key().as_deref() == Some(&k)) {
                self.active = i;
                return;
            }
        }
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }

    fn run_sql(&mut self, sql: &str, ok: &str) {
        match self.db.execute(sql) {
            Ok(_) => {
                self.status = ok.to_string();
                self.error = None;
                self.tabs.iter_mut().for_each(Tab::mark_dirty);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn export_active(&mut self) {
        let data: Option<(Vec<String>, Vec<accessor_core::Row>, String)> = match self.tabs.get(self.active) {
            Some(Tab::Datasheet(d)) => self
                .db
                .query(&format!("SELECT * FROM {}", bracket(&d.table)))
                .ok()
                .map(|r| (r.columns, r.rows, d.table.clone())),
            Some(Tab::Query(q)) => q.results().map(|(c, r)| (c.to_vec(), r.to_vec(), "query".to_string())),
            _ => None,
        };
        let Some((columns, rows, name)) = data else {
            self.error = Some("Open a datasheet, or run a query, before exporting.".into());
            return;
        };
        let Some(file) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .set_file_name(format!("{name}.csv"))
            .save_file()
        else {
            return;
        };
        let res = std::fs::File::create(&file).and_then(|f| {
            let mut w = std::io::BufWriter::new(f);
            csv::write_csv(&mut w, &columns, &rows)?;
            std::io::Write::flush(&mut w)
        });
        match res {
            Ok(()) => self.status = format!("Exported {} rows to {}", rows.len(), file.display()),
            Err(e) => self.error = Some(format!("Export failed: {e}")),
        }
    }

    fn import_csv(&mut self) {
        let Some(file) = rfd::FileDialog::new().add_filter("CSV", &["csv", "txt"]).pick_file() else { return };
        let stem = Path::new(&file)
            .file_stem()
            .map(|s| s.to_string_lossy().replace(['[', ']'], ""))
            .unwrap_or_else(|| "Imported".into());
        match std::fs::read_to_string(&file) {
            Ok(text) => match csv::import_csv_as_table(&mut self.db, &stem, &text) {
                Ok(n) => {
                    self.status = format!("Imported {n} rows into new table '{stem}'");
                    self.open_tab(Tab::Datasheet(Datasheet::new(stem)));
                }
                Err(e) => self.error = Some(format!("Import failed: {e}")),
            },
            Err(e) => self.error = Some(format!("Cannot read file: {e}")),
        }
    }

    // ----- UI pieces ----------------------------------------------------------

    fn menu(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("New database...").clicked() {
                        ui.close();
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("Accessor database", &["adb"])
                            .set_file_name("Database1.adb")
                            .save_file()
                        {
                            self.open_db(p);
                        }
                    }
                    if ui.button("Open...").clicked() {
                        ui.close();
                        if let Some(p) = rfd::FileDialog::new().add_filter("Accessor database", &["adb"]).pick_file() {
                            self.open_db(p);
                        }
                    }
                    ui.separator();
                    if ui.button("Import CSV as new table...").clicked() {
                        ui.close();
                        self.import_csv();
                    }
                    if ui.button("Export current grid to CSV...").clicked() {
                        ui.close();
                        self.export_active();
                    }
                    ui.separator();
                    if ui.button("Load sample data").clicked() {
                        ui.close();
                        self.run_sql(SAMPLE_SQL, "Sample data loaded");
                    }
                    if ui.button("Compact and repair").clicked() {
                        ui.close();
                        match self.db.compact() {
                            Ok(()) => {
                                self.status = "Compacted".into();
                                self.tabs.iter_mut().for_each(Tab::mark_dirty);
                            }
                            Err(e) => self.error = Some(e.to_string()),
                        }
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Create", |ui| {
                    if ui.button("Table").clicked() {
                        ui.close();
                        self.new_table = Some(NewTable::new());
                    }
                    if ui.button("Query").clicked() {
                        ui.close();
                        self.open_tab(Tab::Query(QueryTab::new(None, String::new())));
                    }
                    if ui.button("Parameter query").clicked() {
                        ui.close();
                        let sql = "PARAMETERS @name;
SELECT * FROM TableName WHERE FieldName = @name";
                        self.open_tab(Tab::Query(QueryTab::new(None, sql.to_string())));
                    }
                    if ui.button("Trigger").clicked() {
                        ui.close();
                        let sql = "CREATE TRIGGER TriggerName AFTER INSERT ON TableName
BEGIN
  INSERT INTO LogTable (Msg) VALUES ('added ' & NEW.FieldName);
END";
                        self.open_tab(Tab::Query(QueryTab::new(None, sql.to_string())));
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About").clicked() {
                        ui.close();
                        self.about = true;
                    }
                });
            });
        });
    }

    fn nav_pane(&mut self, ctx: &egui::Context, ev: &mut Vec<Event>) {
        egui::SidePanel::left("nav").resizable(true).default_width(220.0).show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(RichText::new("All Access Objects").strong().color(ACCENT));
            ui.separator();
            let tables: Vec<String> = self.db.table_names().iter().map(|s| s.to_string()).collect();
            let queries: Vec<String> = self.db.view_names().iter().map(|s| s.to_string()).collect();
            let triggers: Vec<String> = self.db.trigger_names().iter().map(|s| s.to_string()).collect();
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::CollapsingHeader::new(RichText::new("Tables").strong()).default_open(true).show(ui, |ui| {
                    for t in &tables {
                        let r = ui.selectable_label(false, t);
                        if r.double_clicked() {
                            ev.push(Event::OpenDatasheet(t.clone()));
                        }
                        r.context_menu(|ui| {
                            if ui.button("Open").clicked() {
                                ev.push(Event::OpenDatasheet(t.clone()));
                                ui.close();
                            }
                            if ui.button("Form view").clicked() {
                                ev.push(Event::OpenForm(t.clone()));
                                ui.close();
                            }
                            if ui.button("Design view").clicked() {
                                ev.push(Event::OpenDesign(t.clone()));
                                ui.close();
                            }
                            if ui.button("Delete...").clicked() {
                                self.confirm = Some(Confirm::DropTable(t.clone()));
                                ui.close();
                            }
                        });
                    }
                    if tables.is_empty() {
                        ui.label(RichText::new("(none)").weak());
                    }
                });
                egui::CollapsingHeader::new(RichText::new("Queries").strong()).default_open(true).show(ui, |ui| {
                    for q in &queries {
                        let sql = self.db.view_sql(q).unwrap_or_default().to_string();
                        let r = ui.selectable_label(false, q);
                        if r.double_clicked() {
                            ev.push(Event::OpenQuery { name: Some(q.clone()), sql: sql.clone() });
                        }
                        r.context_menu(|ui| {
                            if ui.button("Open / edit").clicked() {
                                ev.push(Event::OpenQuery { name: Some(q.clone()), sql: sql.clone() });
                                ui.close();
                            }
                            if ui.button("Delete...").clicked() {
                                self.confirm = Some(Confirm::DropQuery(q.clone()));
                                ui.close();
                            }
                        });
                    }
                    if queries.is_empty() {
                        ui.label(RichText::new("(none)").weak());
                    }
                });
                egui::CollapsingHeader::new(RichText::new("Triggers").strong()).default_open(true).show(ui, |ui| {
                    for t in &triggers {
                        let sql = self.db.trigger_sql(t).unwrap_or_default().to_string();
                        let r = ui.selectable_label(false, t);
                        // A trigger is changed by dropping and re-creating it, so open it as unsaved SQL.
                        if r.double_clicked() {
                            ev.push(Event::OpenQuery { name: None, sql: sql.clone() });
                        }
                        r.context_menu(|ui| {
                            if ui.button("Show definition").clicked() {
                                ev.push(Event::OpenQuery { name: None, sql: sql.clone() });
                                ui.close();
                            }
                            if ui.button("Delete...").clicked() {
                                self.confirm = Some(Confirm::DropTrigger(t.clone()));
                                ui.close();
                            }
                        });
                    }
                    if triggers.is_empty() {
                        ui.label(RichText::new("(none)").weak());
                    }
                });
            });
        });
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for (i, t) in self.tabs.iter().enumerate() {
                if ui.selectable_label(self.active == i, t.title()).clicked() {
                    self.active = i;
                }
                if ui.small_button("x").on_hover_text("Close").clicked() {
                    close = Some(i);
                }
                ui.add_space(8.0);
            }
        });
        if let Some(i) = close {
            self.tabs.remove(i);
            self.active = self.active.min(self.tabs.len().saturating_sub(1));
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.new_table.is_some() {
            let mut open = true;
            let mut created: Option<(String, String)> = None;
            egui::Window::new("New table").open(&mut open).collapsible(false).show(ctx, |ui| {
                if let Some(nt) = &mut self.new_table {
                    if let Some(sql) = nt.ui(ui) {
                        created = Some((nt.name.trim().to_string(), sql));
                    }
                }
            });
            if let Some((name, sql)) = created {
                match self.db.execute(&sql) {
                    Ok(_) => {
                        self.status = format!("Created table '{name}'");
                        self.new_table = None;
                        self.open_tab(Tab::Datasheet(Datasheet::new(name)));
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
            } else if !open {
                self.new_table = None;
            }
        }

        if let Some(c) = &self.confirm {
            let (what, name) = match c {
                Confirm::DropTable(n) => ("table", n.clone()),
                Confirm::DropQuery(n) => ("query", n.clone()),
                Confirm::DropTrigger(n) => ("trigger", n.clone()),
            };
            let mut decision: Option<bool> = None;
            egui::Window::new("Confirm delete").collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label(format!("Permanently delete the {what} '{name}'? This cannot be undone."));
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
            match decision {
                Some(true) => {
                    let sql = match self.confirm.take() {
                        Some(Confirm::DropTable(n)) => format!("DROP TABLE {}", bracket(&n)),
                        Some(Confirm::DropQuery(n)) => format!("DROP VIEW {}", bracket(&n)),
                        Some(Confirm::DropTrigger(n)) => format!("DROP TRIGGER {}", bracket(&n)),
                        None => String::new(),
                    };
                    self.tabs.retain(|t| !t.references(&name));
                    self.active = self.active.min(self.tabs.len().saturating_sub(1));
                    self.run_sql(&sql, &format!("Deleted '{name}'"));
                }
                Some(false) => self.confirm = None,
                None => {}
            }
        }

        if self.about {
            egui::Window::new("About Accessor").collapsible(false).resizable(false).open(&mut self.about).show(ctx, |ui| {
                ui.label("Accessor — a small desktop database in the spirit of Microsoft Access.");
                ui.label("Engine: accessor-core (Rust). Tables, queries, forms, SQL.");
            });
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut ev: Vec<Event> = Vec::new();

        self.menu(ctx);
        self.nav_pane(ctx, &mut ev);

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                match &self.path {
                    Some(p) => ui.label(RichText::new(p.display().to_string()).weak()),
                    None => ui.label(RichText::new("transient in-memory database").weak()),
                };
                ui.separator();
                ui.label(&self.status);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(msg) = self.error.clone() {
                ui.horizontal(|ui| {
                    ui.colored_label(Color32::from_rgb(180, 30, 30), msg);
                    if ui.small_button("dismiss").clicked() {
                        self.error = None;
                    }
                });
                ui.separator();
            }
            if self.tabs.is_empty() {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.heading("Welcome to Accessor");
                    ui.label("Double-click a table or query in the left pane, or use the Create menu.");
                });
                return;
            }
            self.tab_bar(ui);
            ui.separator();
            // Closing the last tab leaves nothing to draw this frame.
            if self.tabs.is_empty() {
                return;
            }
            let active = self.active.min(self.tabs.len() - 1);
            match &mut self.tabs[active] {
                Tab::Datasheet(d) => d.ui(ui, &mut self.db, &mut ev),
                Tab::Form(f) => f.ui(ui, &mut self.db, &mut ev),
                Tab::Design(d) => d.ui(ui, &self.db, &mut ev),
                Tab::Query(q) => q.ui(ui, &mut self.db, &mut ev),
            }
        });

        self.dialogs(ctx);

        // Events change state after this frame was drawn; draw once more to show it.
        if !ev.is_empty() {
            ctx.request_repaint();
        }
        for e in ev {
            match e {
                Event::Changed => {
                    self.error = None;
                    self.tabs.iter_mut().for_each(Tab::mark_dirty);
                }
                Event::Error(m) => self.error = Some(m),
                Event::Execute { sql, ok } => self.run_sql(&sql, &ok),
                Event::Status(m) => self.status = m,
                Event::OpenDatasheet(t) => self.open_tab(Tab::Datasheet(Datasheet::new(t))),
                Event::OpenDesign(t) => self.open_tab(Tab::Design(Design::new(t))),
                Event::OpenForm(t) => self.open_tab(Tab::Form(Form::new(t))),
                Event::OpenQuery { name, sql } => self.open_tab(Tab::Query(QueryTab::new(name, sql))),
            }
        }
    }
}
