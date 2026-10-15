#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anticheat;
mod app;
mod download;
mod gpu;
mod launchers;
mod manifest;
mod patcher;
mod steam;

use eframe::egui;

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("DLSSG Patcher")
        .with_inner_size([980.0, 660.0])
        .with_min_inner_size([760.0, 500.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "DLSSG Patcher",
        options,
        Box::new(|cc| Ok(Box::new(app::DlssgApp::new(cc)))),
    )
}