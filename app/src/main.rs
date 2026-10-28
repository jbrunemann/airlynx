#[cfg(not(target_os = "android"))]
fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("AirLynx")
            .with_inner_size([440.0, 560.0])
            .with_min_inner_size([360.0, 420.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "AirLynx",
        options,
        Box::new(|cc| Ok(Box::new(airlynx_app::AirLynxApp::new(cc)))),
    )
}

#[cfg(target_os = "android")]
fn main() {}
