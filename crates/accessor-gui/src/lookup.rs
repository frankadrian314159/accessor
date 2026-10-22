//! Lookup lists for fields that refer to another table.
//!
//! A field that is the single column of a relationship is shown as a
//! drop-down of the referenced table's records, labelled with its first text
//! field (or the key itself when it has none).

use std::collections::HashMap;

use accessor_core::{DataType, Database, Value};
use egui::Ui;

pub struct Lookup {
    pub parent: String,
    /// `(key, label)` ordered by label.
    pub options: Vec<(Value, String)>,
    labels: HashMap<Value, usize>,
}

impl Lookup {
    pub fn label(&self, key: &Value) -> Option<&str> {
        self.labels.get(key).map(|&i| self.options[i].1.as_str())
    }
}

/// One entry per column of `table`; `Some` where the column is a lookup.
/// Cost is proportional to the referenced tables' sizes, so callers cache it
/// until the data changes.
pub fn build(db: &Database, table: &str) -> Vec<Option<Lookup>> {
    let Some(t) = db.table(table) else { return Vec::new() };
    let schema = t.schema();
    (0..schema.columns.len())
        .map(|c| {
            let fk = schema.foreign_keys.iter().find(|f| f.columns == [c] && f.ref_columns.len() == 1)?;
            let parent = db.table(&fk.ref_table)?;
            let key_col = fk.ref_columns[0];
            let shown = parent
                .schema()
                .columns
                .iter()
                .enumerate()
                .find(|(i, col)| *i != key_col && col.data_type == DataType::Text)
                .map(|(i, _)| i);
            let mut options: Vec<(Value, String)> = parent
                .iter()
                .map(|(_, r)| (r[key_col].clone(), shown.map_or_else(|| r[key_col].to_string(), |s| r[s].to_string())))
                .collect();
            options.sort_by_cached_key(|(_, label)| label.to_lowercase());
            let labels = options.iter().enumerate().map(|(i, (k, _))| (k.clone(), i)).collect();
            Some(Lookup { parent: parent.name().to_string(), options, labels })
        })
        .collect()
}

/// A drop-down over `lookup`. Returns the newly chosen key (`Null` for "none").
pub fn combo(ui: &mut Ui, id: impl std::hash::Hash, lookup: &Lookup, current: &Value, width: f32) -> Option<Value> {
    let text = if current.is_null() {
        String::new()
    } else {
        lookup.label(current).map_or_else(|| format!("{current} (missing)"), str::to_string)
    };
    let mut chosen = None;
    egui::ComboBox::from_id_salt(id).selected_text(text).width(width).show_ui(ui, |ui| {
        if ui.selectable_label(current.is_null(), "(none)").clicked() {
            chosen = Some(Value::Null);
        }
        let row_h = ui.text_style_height(&egui::TextStyle::Button) + ui.spacing().item_spacing.y;
        egui::ScrollArea::vertical().max_height(300.0).show_rows(ui, row_h, lookup.options.len(), |ui, range| {
            for (key, label) in &lookup.options[range] {
                if ui.selectable_label(key == current, label).clicked() {
                    chosen = Some(key.clone());
                }
            }
        });
    });
    chosen.filter(|c| c != current)
}
