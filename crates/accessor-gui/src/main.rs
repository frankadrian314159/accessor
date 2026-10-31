//! Accessor desktop application.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod datasheet;
mod design;
mod form;
mod lookup;
mod query;
mod sample;
mod ui;

fn main() -> Result<(), eframe::Error> {
    // `accessor-gui [--asql|--postgresql|--mysql] [FILE.adb]`: the dialect is asql unless one is named
    let mut dialect = accessor_core::dialect::Dialect::Asql;
    let mut path = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--asql" => dialect = accessor_core::dialect::Dialect::Asql,
            "--postgresql" => dialect = accessor_core::dialect::Dialect::Postgresql,
            "--mysql" => dialect = accessor_core::dialect::Dialect::Mysql,
            _ => path = Some(arg),
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([640.0, 420.0])
            .with_title("Accessor"),
        ..Default::default()
    };
    eframe::run_native("Accessor", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, path, dialect)))))
}
