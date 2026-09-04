//! The on-screen volume popup. It is the *root* eframe window: always mapped,
//! transparent, click-through, topmost; it paints nothing while idle.
//! Implemented by WP3 (this file holds the contract plus a crude placeholder).

use std::time::{Duration, Instant};

use knobify_core::spotify::UserFacing;
use knobify_core::OsdSettings;

pub const OSD_SIZE: egui::Vec2 = egui::vec2(320.0, 72.0);

#[derive(Debug, Clone, PartialEq)]
pub enum OsdContent {
    Volume { percent: u8, muted: bool, pending: bool },
    Message { title: String, detail: String, kind: MsgKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Info,
    Warning,
    Error,
}

impl From<&UserFacing> for OsdContent {
    fn from(e: &UserFacing) -> Self {
        let kind = match e {
            UserFacing::RateLimited { .. } | UserFacing::NoActiveDevice => MsgKind::Warning,
            _ => MsgKind::Error,
        };
        OsdContent::Message {
            title: e.title().to_owned(),
            detail: e.to_string(),
            kind,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OsdState {
    pub content: OsdContent,
    /// `Some` while the popup should be drawn.
    pub hide_at: Option<Instant>,
    /// Re-send the window position on the next frame.
    pub needs_reposition: bool,
}

impl Default for OsdState {
    fn default() -> Self {
        Self {
            content: OsdContent::Volume {
                percent: 50,
                muted: false,
                pending: false,
            },
            hide_at: None,
            needs_reposition: true,
        }
    }
}

impl OsdState {
    /// Show `content` (or extend the visible period) for `duration`.
    pub fn show(&mut self, content: OsdContent, duration: Duration, now: Instant) {
        self.content = content;
        self.hide_at = Some(now + duration);
    }

    pub fn is_visible(&self, now: Instant) -> bool {
        self.hide_at.is_some_and(|t| now < t)
    }

    /// Called from `App::logic` every pass: expire, and schedule the next repaint.
    pub fn tick(&mut self, ctx: &egui::Context, now: Instant) {
        match self.hide_at {
            Some(hide_at) if now >= hide_at => {
                self.hide_at = None;
                ctx.request_repaint();
            }
            Some(hide_at) => ctx.request_repaint_after(hide_at - now),
            None => {}
        }
    }
}

/// Root window attributes for the popup.
pub fn root_viewport_builder(osd: &OsdSettings) -> egui::ViewportBuilder {
    let mut builder = egui::ViewportBuilder::default()
        .with_title("Knobify")
        .with_decorations(false)
        .with_resizable(false)
        .with_always_on_top()
        .with_mouse_passthrough(true)
        .with_taskbar(false)
        .with_active(false)
        .with_inner_size(OSD_SIZE)
        .with_transparent(osd.transparent);
    if let Ok(icon) = crate::icon::egui_icon() {
        builder = builder.with_icon(icon);
    }
    builder
}

/// Compute the outer position (logical points) for the popup on the primary
/// monitor's work area. `None` when geometry is unavailable.
pub fn compute_position(
    frame: &eframe::Frame,
    ctx: &egui::Context,
    osd: &OsdSettings,
) -> Option<egui::Pos2> {
    let _ = (frame, ctx, osd);
    todo!("WP3: primary monitor + work area -> OsdPosition with margin")
}

/// Paint the popup into the root viewport. Draws nothing when not visible.
pub fn draw(ui: &mut egui::Ui, state: &OsdState, osd: &OsdSettings, now: Instant) {
    if !state.is_visible(now) {
        return;
    }
    // Placeholder look; WP3 replaces this with the real flyout.
    let rect = ui.max_rect();
    let painter = ui.painter();
    painter.rect_filled(rect, 12.0, egui::Color32::from_black_alpha(200));
    let text = match &state.content {
        OsdContent::Volume { percent, muted: true, .. } => format!("Muted ({percent}%)"),
        OsdContent::Volume { percent, .. } => format!("Volume {percent}%"),
        OsdContent::Message { title, detail, .. } => format!("{title}: {detail}"),
    };
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(18.0),
        egui::Color32::WHITE,
    );
    let _ = osd;
}
