//! The application icon, embedded at compile time.

use anyhow::Context;

const ICON_BYTES: &[u8] = include_bytes!("../assets/icon.ico");

pub struct Rgba {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Decode the largest frame of the embedded `.ico` into RGBA8.
pub fn rgba() -> anyhow::Result<Rgba> {
    let image = image::load_from_memory_with_format(ICON_BYTES, image::ImageFormat::Ico)
        .context("decoding embedded icon.ico")?
        .into_rgba8();
    let (width, height) = image.dimensions();
    Ok(Rgba {
        pixels: image.into_raw(),
        width,
        height,
    })
}

pub fn tray_icon() -> anyhow::Result<tray_icon::Icon> {
    let icon = rgba()?;
    tray_icon::Icon::from_rgba(icon.pixels, icon.width, icon.height).context("building tray icon")
}

pub fn egui_icon() -> anyhow::Result<egui::IconData> {
    let icon = rgba()?;
    Ok(egui::IconData {
        rgba: icon.pixels,
        width: icon.width,
        height: icon.height,
    })
}
