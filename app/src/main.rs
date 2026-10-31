#[cfg(not(target_os = "android"))]
fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("AirLynx")
            // The window class on X11 and the app id on Wayland: how the desktop matches this window to airlynx.desktop
            // (StartupWMClass) and so shows its icon.
            .with_app_id("airlynx")
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon/airlynx.png")).expect("valid icon"))
            .with_inner_size([880.0, 620.0])
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
