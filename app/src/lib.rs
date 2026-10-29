mod app;
mod platform;
mod remote;
mod theme;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
mod browser;
#[cfg(target_os = "android")]
mod diag;

pub use app::AirLynxApp;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info));
    android::init(app.clone());
    android::exit_when_destroyed();

    let options = eframe::NativeOptions {
        android_app: Some(app),
        ..Default::default()
    };
    if let Err(e) = eframe::run_native("AirLynx", options, Box::new(|cc| Ok(Box::new(AirLynxApp::new(cc))))) {
        log::error!("eframe exited with error: {e}");
    }
    // The app (and its receiver) is gone, so the "ready to receive" notification must go too.
    android::set_keep_awake(false);
}
