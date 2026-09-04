//! Knobify: control Spotify volume with a keyboard knob.
//!
//! Windows-only. The root eframe window *is* the on-screen volume popup
//! (transparent, click-through, always on top); Settings is a child viewport;
//! a tray icon hosts the menu. See `app.rs` for the wiring.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod handle;
mod hotkeys;
mod icon;
mod ui;
mod win;

use anyhow::{anyhow, Context};
use knobify_core::config;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cfg_path = config::config_path().context("locating the config directory")?;
    let settings = match config::load(&cfg_path) {
        Ok(settings) => settings,
        Err(e) => {
            log::error!("{e}; starting with default settings (the file is left untouched)");
            config::Settings::default()
        }
    };

    let native_options = eframe::NativeOptions {
        viewport: ui::osd::root_viewport_builder(&settings.osd),
        renderer: eframe::Renderer::Glow,
        persist_window: false,
        centered: false,
        ..Default::default()
    };

    eframe::run_native(
        "Knobify",
        native_options,
        Box::new(move |cc| {
            let app = app::KnobifyApp::new(cc, settings, cfg_path)?;
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow!("eframe failed: {e}"))
}
