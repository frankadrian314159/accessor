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
    let path = std::env::args().nth(1);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([640.0, 420.0])
            .with_title("Accessor"),
        ..Default::default()
    };
    eframe::run_native("Accessor", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, path)))))
}
