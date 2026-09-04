//! The on-screen volume popup. It is the *root* eframe window: always mapped,
//! transparent, click-through, topmost; it paints nothing while idle.
//!
//! # Why the root window is never hidden (transparent mode)
//!
//! eframe runs only `App::logic` for a root viewport that is not being shown
//! and processes only viewport *commands* there, so a hidden root can neither
//! create nor show the Settings child viewport, and its repaints are throttled
//! to a 100 ms heartbeat. Therefore the transparent popup stays mapped forever
//! and simply paints nothing while `hide_at` is `None`; `clear_color` is fully
//! transparent, so an empty pass is an invisible window.
//!
//! The opaque fallback (`osd.transparent == false`, for drivers without WGL
//! alpha) has no such trick available: an empty pass would be a dark grey
//! rectangle. There `tick` hides and shows the root with
//! `ViewportCommand::Visible`, and keeps it mapped while Settings is open so
//! the child viewport can still be created.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, FontId, Galley, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2, pos2, vec2,
};
use knobify_core::spotify::UserFacing;
use knobify_core::{OsdPosition, OsdSettings};

pub const OSD_SIZE: egui::Vec2 = egui::vec2(320.0, 72.0);

/// Points of `OSD_SIZE` reserved around the panel for the drop shadow.
const SHADOW_PAD: f32 = 7.0;
/// Panel corner radius, in points.
const CORNER: f32 = 12.0;
/// Horizontal padding inside the panel.
const PAD_X: f32 = 14.0;

const FADE_IN: Duration = Duration::from_millis(120);
const FADE_OUT: Duration = Duration::from_millis(150);
/// Repaint interval while a fade is running (~60 Hz).
const ANIM_STEP: Duration = Duration::from_millis(16);

const PANEL_FILL_TRANSPARENT: Color32 = Color32::from_rgba_unmultiplied_const(32, 32, 32, 230);
const PANEL_FILL_OPAQUE: Color32 = Color32::from_rgb(0x20, 0x20, 0x20);
const PANEL_STROKE: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 20);
const TRACK_BG: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 60);
const ACCENT: Color32 = Color32::from_rgb(30, 215, 96);
const ACCENT_MUTED: Color32 = Color32::from_rgb(130, 130, 130);

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

impl MsgKind {
    fn color(self) -> Color32 {
        match self {
            MsgKind::Info => Color32::from_rgb(0x4c, 0xa0, 0xff),
            MsgKind::Warning => Color32::from_rgb(0xff, 0xb9, 0x00),
            MsgKind::Error => Color32::from_rgb(0xe8, 0x4c, 0x4c),
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            MsgKind::Info => "i",
            MsgKind::Warning => "!",
            MsgKind::Error => "x",
        }
    }
}

impl From<&UserFacing> for OsdContent {
    fn from(e: &UserFacing) -> Self {
        let kind = match e {
            // Not a failure, just something the user has to do first.
            UserFacing::NotLoggedIn => MsgKind::Info,
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
    /// When the current visible period started (drives the fade-in).
    shown_at: Option<Instant>,
    /// Opaque fallback only: the visibility we last asked the root window for.
    /// eframe maps the root itself after the first painted frame, hence `true`.
    window_shown: bool,
    /// Opaque fallback only: winit rewrites the whole `GWL_EXSTYLE` word when it
    /// toggles visibility, wiping `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`; re-add
    /// them on the pass after the command was handed to winit.
    restyle_pending: bool,
    /// Passes seen so far, saturating at 2. eframe keeps the root window hidden
    /// until it has painted once, so we must not fight it on the first pass.
    passes: u8,
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
            shown_at: None,
            window_shown: true,
            restyle_pending: false,
            passes: 0,
        }
    }
}

impl OsdState {
    /// Show `content` (or extend the visible period) for `duration`.
    pub fn show(&mut self, content: OsdContent, duration: Duration, now: Instant) {
        if !self.is_visible(now) {
            // First show after an idle period: the monitor layout, the work
            // area (taskbar auto-hide) or the DPI may have changed since the
            // last time we placed the window.
            self.needs_reposition = true;
            self.shown_at = Some(now);
        }
        self.content = content;
        self.hide_at = Some(now + duration);
    }

    pub fn is_visible(&self, now: Instant) -> bool {
        self.hide_at.is_some_and(|t| now < t)
    }

    /// Opacity of the whole popup in `0.0..=1.0`: a fade-in from `shown_at` and
    /// a fade-out that ends exactly at `hide_at`. Always `1.0` in the opaque
    /// fallback, where there is no alpha to fade against.
    fn opacity(&self, osd: &OsdSettings, now: Instant) -> f32 {
        let Some(hide_at) = self.hide_at else {
            return 0.0;
        };
        if now >= hide_at {
            return 0.0;
        }
        if !osd.transparent {
            return 1.0;
        }

        let mut alpha = 1.0f32;
        if let Some(shown_at) = self.shown_at {
            let since = now.saturating_duration_since(shown_at);
            if since < FADE_IN {
                alpha = since.as_secs_f32() / FADE_IN.as_secs_f32();
            }
        }
        let remaining = hide_at.saturating_duration_since(now);
        if remaining < FADE_OUT {
            alpha = alpha.min(remaining.as_secs_f32() / FADE_OUT.as_secs_f32());
        }
        alpha.clamp(0.0, 1.0)
    }

    /// Is a fade animation running right now (so we need per-frame repaints)?
    fn fading(&self, osd: &OsdSettings, now: Instant) -> bool {
        if !osd.transparent {
            return false;
        }
        let Some(hide_at) = self.hide_at else {
            return false;
        };
        if now >= hide_at {
            return false;
        }
        let fading_in = self
            .shown_at
            .is_some_and(|shown_at| now.saturating_duration_since(shown_at) < FADE_IN);
        fading_in || hide_at.saturating_duration_since(now) <= FADE_OUT
    }

    /// How long until the popup needs painting again: one animation step while
    /// a fade is running, otherwise straight to the next interesting moment
    /// (the start of the fade-out, or expiry). Never a busy loop.
    fn repaint_delay(&self, osd: &OsdSettings, hide_at: Instant, now: Instant) -> Duration {
        let remaining = hide_at.saturating_duration_since(now);
        if self.fading(osd, now) {
            remaining.min(ANIM_STEP)
        } else if osd.transparent && remaining > FADE_OUT {
            remaining - FADE_OUT
        } else {
            remaining
        }
    }

    /// Called from `App::logic` every pass: expire, keep the root window's
    /// visibility in sync (opaque fallback) and schedule the next repaint.
    ///
    /// `keep_visible` must be true while the Settings viewport is open: a
    /// hidden root cannot show a child viewport.
    pub fn tick(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        osd: &OsdSettings,
        keep_visible: bool,
        now: Instant,
    ) {
        self.passes = self.passes.saturating_add(1).min(2);

        match self.hide_at {
            Some(hide_at) if now >= hide_at => {
                self.hide_at = None;
                self.shown_at = None;
                // One more pass to paint the popup away.
                ctx.request_repaint();
            }
            Some(hide_at) => ctx.request_repaint_after(self.repaint_delay(osd, hide_at, now)),
            // Idle: no repaints at all, so the process costs nothing.
            None => {}
        }

        self.sync_window_visibility(ctx, frame, osd, keep_visible);
    }

    /// Opaque fallback: map/unmap the root window instead of painting nothing.
    fn sync_window_visibility(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
        osd: &OsdSettings,
        keep_visible: bool,
    ) {
        if std::mem::take(&mut self.restyle_pending) {
            if let Some(hwnd) = root_hwnd(frame) {
                crate::win::apply_osd_exstyles(hwnd);
            }
        }

        if osd.transparent {
            // The transparent root stays mapped forever (see the module docs).
            return;
        }

        let want = keep_visible || self.hide_at.is_some();
        if want == self.window_shown {
            return;
        }
        if !want && self.passes < 2 {
            // eframe unhides the root only after its first painted frame; a
            // Visible(false) sent before that would be undone right away.
            return;
        }

        self.window_shown = want;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(want));
        if want {
            self.needs_reposition = true;
            self.restyle_pending = true;
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
        .with_transparent(osd.transparent)
        // eframe forces `with_visible(false)` on the root and unhides it after
        // the first painted frame, in both modes. Asking for `true` keeps the
        // intent explicit and matches what the window ends up as.
        .with_visible(true);
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
    let window = frame.winit_window()?;
    let monitor = window
        .primary_monitor()
        .or_else(|| window.current_monitor())?;

    // `ViewportCommand::OuterPosition` is multiplied by egui's own
    // pixels-per-point before it reaches winit, so that is the factor we must
    // divide the physical target position by - not the monitor's scale factor.
    let mut ppp = ctx.pixels_per_point();
    if !ppp.is_finite() || ppp <= 0.0 {
        ppp = monitor.scale_factor() as f32;
    }
    if !ppp.is_finite() || ppp <= 0.0 {
        ppp = 1.0;
    }

    // The work area excludes the taskbar; fall back to the whole monitor.
    let work = crate::win::primary_work_area().unwrap_or_else(|| {
        let origin = monitor.position();
        let size = monitor.size();
        crate::win::Rect {
            left: origin.x,
            top: origin.y,
            right: origin.x.saturating_add(size.width as i32),
            bottom: origin.y.saturating_add(size.height as i32),
        }
    });

    Some(place(work, OSD_SIZE, osd.margin, ppp, osd.position))
}

/// Pure layout math: place an `size_pt` box inside the physical-pixel rectangle
/// `work`, `margin_pt` points away from the edges `position` names, and return
/// its top-left corner **in points**.
fn place(
    work: crate::win::Rect,
    size_pt: Vec2,
    margin_pt: f32,
    ppp: f32,
    position: OsdPosition,
) -> Pos2 {
    let ppp = if ppp.is_finite() && ppp > 0.0 {
        ppp
    } else {
        1.0
    };
    let margin_pt = if margin_pt.is_finite() && margin_pt >= 0.0 {
        margin_pt
    } else {
        0.0
    };

    let left = work.left as f32;
    let top = work.top as f32;
    let width = work.width() as f32;
    let height = work.height() as f32;

    let size = size_pt * ppp;
    let margin = margin_pt * ppp;

    let x = match position {
        OsdPosition::TopLeft | OsdPosition::BottomLeft => left + margin,
        OsdPosition::TopCenter | OsdPosition::BottomCenter => left + (width - size.x) * 0.5,
        OsdPosition::TopRight | OsdPosition::BottomRight => left + width - size.x - margin,
    };
    let y = match position {
        OsdPosition::TopLeft | OsdPosition::TopCenter | OsdPosition::TopRight => top + margin,
        OsdPosition::BottomLeft | OsdPosition::BottomCenter | OsdPosition::BottomRight => {
            top + height - size.y - margin
        }
    };

    // A margin larger than the screen must not push the popup off it.
    let x = x.clamp(left, (left + width - size.x).max(left));
    let y = y.clamp(top, (top + height - size.y).max(top));

    pos2(x / ppp, y / ppp)
}

/// Paint the popup into the root viewport. Draws nothing when not visible.
pub fn draw(ui: &mut egui::Ui, state: &OsdState, osd: &OsdSettings, now: Instant) {
    if !state.is_visible(now) {
        return;
    }
    let alpha = state.opacity(osd, now);
    if alpha <= 0.0 {
        return;
    }

    let painter = ui.painter().clone();
    // Transparent mode: inset the panel so the drop shadow has room, and round
    // it ourselves. Opaque mode: fill the whole window (there is no shadow to
    // make room for) and let DWM round the window's own corners.
    let (panel, corner, fill) = if osd.transparent {
        (
            ui.max_rect().shrink(SHADOW_PAD),
            CORNER,
            PANEL_FILL_TRANSPARENT.gamma_multiply(alpha),
        )
    } else {
        (ui.max_rect(), 0.0, PANEL_FILL_OPAQUE)
    };
    if panel.width() < 32.0 || panel.height() < 16.0 {
        return;
    }

    if osd.transparent {
        let shadow = egui::Shadow {
            offset: [0, 2],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(90).gamma_multiply(alpha),
        };
        painter.add(shadow.as_shape(panel, corner));
    }

    painter.rect_filled(panel, corner, fill);
    painter.rect_stroke(
        panel,
        corner,
        Stroke::new(1.0, PANEL_STROKE.gamma_multiply(alpha)),
        StrokeKind::Inside,
    );

    let inner = Rect::from_min_max(
        pos2(panel.left() + PAD_X, panel.top()),
        pos2(panel.right() - PAD_X, panel.bottom()),
    );
    if inner.width() < 24.0 {
        return;
    }

    match &state.content {
        OsdContent::Volume {
            percent,
            muted,
            pending,
        } => draw_volume(&painter, inner, *percent, *muted, *pending, alpha),
        OsdContent::Message {
            title,
            detail,
            kind,
        } => draw_message(&painter, inner, title, detail, *kind, alpha),
    }
}

fn draw_volume(
    painter: &egui::Painter,
    inner: Rect,
    percent: u8,
    muted: bool,
    pending: bool,
    alpha: f32,
) {
    let percent = percent.min(100);
    let white = Color32::WHITE.gamma_multiply(alpha);
    let accent = if muted { ACCENT_MUTED } else { ACCENT }.gamma_multiply(alpha);

    // Left: the speaker glyph on a 24 pt square.
    let icon = Rect::from_center_size(
        pos2(inner.left() + 12.0, inner.center().y),
        Vec2::splat(24.0),
    );
    let level = match percent {
        0 => 0,
        1..=33 => 1,
        34..=66 => 2,
        _ => 3,
    };
    draw_speaker(painter, icon, level, muted, white);

    // Right: the readout. Reserve the width of the widest label plus the
    // pending dot so the track never jitters as the number changes.
    let font = FontId::proportional(18.0);
    let label = if muted {
        "Muted".to_owned()
    } else {
        format!("{percent}%")
    };
    let galley = painter.layout_no_wrap(label, font.clone(), white);
    let reserved = ["100%", "Muted"]
        .iter()
        .map(|s| {
            painter
                .layout_no_wrap((*s).to_owned(), font.clone(), white)
                .size()
                .x
        })
        .fold(galley.size().x, f32::max);
    const DOT_SPACE: f32 = 10.0;
    let text_left = inner.right() - DOT_SPACE - reserved;
    painter.galley(
        pos2(
            inner.right() - DOT_SPACE - galley.size().x,
            inner.center().y - galley.size().y * 0.5,
        ),
        galley,
        white,
    );
    if pending {
        painter.circle_filled(
            pos2(inner.right() - DOT_SPACE * 0.5, inner.center().y),
            2.0,
            white.gamma_multiply(0.5),
        );
    }

    // Middle: the track.
    let track_left = icon.right() + 12.0;
    let track_right = text_left - 12.0;
    if track_right - track_left < 8.0 {
        return;
    }
    let track = Rect::from_min_max(
        pos2(track_left, inner.center().y - 2.0),
        pos2(track_right, inner.center().y + 2.0),
    );
    painter.rect_filled(track, 2.0, TRACK_BG.gamma_multiply(alpha));
    let filled = track.width() * f32::from(percent) / 100.0;
    if filled >= 1.0 {
        painter.rect_filled(
            Rect::from_min_size(track.min, vec2(filled, track.height())),
            2.0,
            accent,
        );
    }
}

fn draw_message(
    painter: &egui::Painter,
    inner: Rect,
    title: &str,
    detail: &str,
    kind: MsgKind,
    alpha: f32,
) {
    let white = Color32::WHITE.gamma_multiply(alpha);
    let dim = Color32::from_rgb(200, 200, 200).gamma_multiply(alpha);

    let center = pos2(inner.left() + 10.0, inner.center().y);
    painter.circle_filled(center, 10.0, kind.color().gamma_multiply(alpha));
    painter.text(
        center,
        Align2::CENTER_CENTER,
        kind.glyph(),
        FontId::proportional(12.0),
        Color32::from_rgba_unmultiplied(0, 0, 0, 200).gamma_multiply(alpha),
    );

    let text_left = inner.left() + 32.0;
    let max_width = (inner.right() - text_left).max(0.0);
    if max_width < 16.0 {
        return;
    }

    let title = truncated(painter, title, FontId::proportional(15.0), white, max_width);
    let detail = truncated(painter, detail, FontId::proportional(12.0), dim, max_width);
    let gap = 2.0;
    let title_height = title.size().y;
    let total = title_height + gap + detail.size().y;
    let top = inner.center().y - total * 0.5;

    // The default fonts ship no bold face, so fake the weight with a second
    // pass offset by a fraction of a point.
    let title_pos = pos2(text_left, top);
    painter.galley(title_pos, Arc::clone(&title), white);
    painter.galley(title_pos + vec2(0.6, 0.0), title, white);
    painter.galley(pos2(text_left, top + title_height + gap), detail, dim);
}

/// Lay out a single line, eliding with `…` when it does not fit `max_width`.
fn truncated(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping {
        max_width: max_width.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}

/// A Windows-style speaker with `level` (0..=3) sound arcs, or an X when muted.
fn draw_speaker(painter: &egui::Painter, rect: Rect, level: u8, muted: bool, color: Color32) {
    let c = rect.center();
    // The glyph is designed on a 24 pt grid and scaled from there.
    let s = rect.width().min(rect.height()) / 24.0;

    let body = Rect::from_min_max(
        pos2(c.x - 9.0 * s, c.y - 3.0 * s),
        pos2(c.x - 4.0 * s, c.y + 3.0 * s),
    );
    painter.rect_filled(body, s, color);

    // The cone: a convex trapezoid, so `convex_polygon` tessellates correctly.
    painter.add(Shape::convex_polygon(
        vec![
            pos2(c.x - 4.5 * s, c.y - 3.0 * s),
            pos2(c.x + 1.0 * s, c.y - 8.5 * s),
            pos2(c.x + 1.0 * s, c.y + 8.5 * s),
            pos2(c.x - 4.5 * s, c.y + 3.0 * s),
        ],
        color,
        Stroke::NONE,
    ));

    if muted {
        let stroke = Stroke::new(1.6 * s, color);
        let (x0, x1) = (c.x + 3.5 * s, c.x + 9.5 * s);
        let (y0, y1) = (c.y - 3.0 * s, c.y + 3.0 * s);
        painter.line_segment([pos2(x0, y0), pos2(x1, y1)], stroke);
        painter.line_segment([pos2(x0, y1), pos2(x1, y0)], stroke);
        return;
    }

    for i in 0..level.min(3) {
        let radius = (4.0 + 3.0 * (f32::from(i) + 1.0)) * s;
        arc(
            painter,
            pos2(c.x + 0.5 * s, c.y),
            radius,
            Stroke::new(1.5 * s, color),
        );
    }
}

/// A polyline approximating an arc centred on the +x axis.
fn arc(painter: &egui::Painter, center: Pos2, radius: f32, stroke: Stroke) {
    const STEPS: usize = 12;
    const HALF_SWEEP: f32 = 0.85; // radians

    let points: Vec<Pos2> = (0..=STEPS)
        .map(|i| {
            let t = -HALF_SWEEP + 2.0 * HALF_SWEEP * (i as f32) / (STEPS as f32);
            center + vec2(t.cos() * radius, t.sin() * radius)
        })
        .collect();
    painter.add(Shape::line(points, stroke));
}

/// The root window's `HWND`, if this build has one.
fn root_hwnd(frame: &eframe::Frame) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

    let window = frame.winit_window()?;
    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(win32) => Some(win32.hwnd.get()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::Rect as PxRect;

    const FHD: PxRect = PxRect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040, // 1080 minus a 40 px taskbar
    };
    const SIZE: Vec2 = OSD_SIZE;

    fn at(work: PxRect, ppp: f32, position: OsdPosition) -> (f32, f32) {
        let p = place(work, SIZE, 48.0, ppp, position);
        (p.x, p.y)
    }

    #[test]
    fn six_positions_at_1x() {
        // ppp = 1, so points and pixels coincide.
        assert_eq!(at(FHD, 1.0, OsdPosition::TopLeft), (48.0, 48.0));
        assert_eq!(at(FHD, 1.0, OsdPosition::TopCenter), (800.0, 48.0));
        assert_eq!(at(FHD, 1.0, OsdPosition::TopRight), (1552.0, 48.0));
        assert_eq!(at(FHD, 1.0, OsdPosition::BottomLeft), (48.0, 920.0));
        assert_eq!(at(FHD, 1.0, OsdPosition::BottomCenter), (800.0, 920.0));
        assert_eq!(at(FHD, 1.0, OsdPosition::BottomRight), (1552.0, 920.0));
    }

    #[test]
    fn six_positions_at_1_5x() {
        // The returned position is in points: the 1920x1040 px work area is
        // 1280x693.33 pt, and the 320x72 pt popup is 480x108 px.
        let (x, y) = at(FHD, 1.5, OsdPosition::TopLeft);
        assert!((x - 48.0).abs() < 0.01, "{x}");
        assert!((y - 48.0).abs() < 0.01, "{y}");

        let (x, y) = at(FHD, 1.5, OsdPosition::BottomRight);
        assert!((x - (1280.0 - 320.0 - 48.0)).abs() < 0.01, "{x}");
        assert!((y - (1040.0 / 1.5 - 72.0 - 48.0)).abs() < 0.01, "{y}");

        let (x, _) = at(FHD, 1.5, OsdPosition::TopCenter);
        assert!((x - (1280.0 - 320.0) / 2.0).abs() < 0.01, "{x}");

        // Vertical placement does not depend on the horizontal anchor.
        for position in [
            OsdPosition::TopLeft,
            OsdPosition::TopCenter,
            OsdPosition::TopRight,
        ] {
            assert!((at(FHD, 1.5, position).1 - 48.0).abs() < 0.01);
        }
        for position in [
            OsdPosition::BottomLeft,
            OsdPosition::BottomCenter,
            OsdPosition::BottomRight,
        ] {
            let expected = 1040.0 / 1.5 - 72.0 - 48.0;
            assert!((at(FHD, 1.5, position).1 - expected).abs() < 0.01);
        }
    }

    #[test]
    fn monitor_with_a_non_zero_origin() {
        // A secondary-style work area to the left of and above the origin.
        let work = PxRect {
            left: -1920,
            top: -200,
            right: 0,
            bottom: 880,
        };
        assert_eq!(at(work, 1.0, OsdPosition::TopLeft), (-1872.0, -152.0));
        assert_eq!(at(work, 1.0, OsdPosition::TopCenter), (-1120.0, -152.0));
        assert_eq!(at(work, 1.0, OsdPosition::TopRight), (-368.0, -152.0));
        assert_eq!(at(work, 1.0, OsdPosition::BottomLeft), (-1872.0, 760.0));
        assert_eq!(at(work, 1.0, OsdPosition::BottomCenter), (-1120.0, 760.0));
        assert_eq!(at(work, 1.0, OsdPosition::BottomRight), (-368.0, 760.0));
    }

    #[test]
    fn all_positions_stay_inside_the_work_area() {
        for &position in &OsdPosition::ALL {
            for ppp in [1.0, 1.25, 1.5, 2.0] {
                let p = place(FHD, SIZE, 48.0, ppp, position);
                let px = pos2(p.x * ppp, p.y * ppp);
                assert!(px.x >= FHD.left as f32 - 0.01, "{position:?} {ppp} {px:?}");
                assert!(px.y >= FHD.top as f32 - 0.01, "{position:?} {ppp} {px:?}");
                assert!(
                    px.x + SIZE.x * ppp <= FHD.right as f32 + 0.01,
                    "{position:?} {ppp} {px:?}"
                );
                assert!(
                    px.y + SIZE.y * ppp <= FHD.bottom as f32 + 0.01,
                    "{position:?} {ppp} {px:?}"
                );
            }
        }
    }

    #[test]
    fn an_absurd_margin_is_clamped_into_the_work_area() {
        let p = place(FHD, SIZE, 5000.0, 1.0, OsdPosition::BottomRight);
        assert_eq!((p.x, p.y), (0.0, 0.0));
        let p = place(FHD, SIZE, 5000.0, 1.0, OsdPosition::TopLeft);
        assert_eq!((p.x, p.y), (1600.0, 968.0));
    }

    #[test]
    fn a_bad_ppp_falls_back_to_1x() {
        let sane = place(FHD, SIZE, 48.0, 1.0, OsdPosition::BottomCenter);
        for bad in [0.0, -2.0, f32::NAN, f32::INFINITY] {
            assert_eq!(place(FHD, SIZE, 48.0, bad, OsdPosition::BottomCenter), sane);
        }
    }

    #[test]
    fn opacity_fades_in_and_out_and_ends_invisible() {
        let osd = OsdSettings::default();
        let now = Instant::now();
        let mut state = OsdState::default();
        state.show(
            OsdContent::Volume {
                percent: 40,
                muted: false,
                pending: false,
            },
            Duration::from_millis(1500),
            now,
        );

        assert_eq!(state.opacity(&osd, now), 0.0);
        assert!(state.fading(&osd, now));
        let mid_fade_in = state.opacity(&osd, now + Duration::from_millis(60));
        assert!((0.4..0.6).contains(&mid_fade_in), "{mid_fade_in}");
        assert_eq!(state.opacity(&osd, now + Duration::from_millis(400)), 1.0);
        assert!(!state.fading(&osd, now + Duration::from_millis(400)));

        let mid_fade_out = state.opacity(&osd, now + Duration::from_millis(1425));
        assert!((0.4..0.6).contains(&mid_fade_out), "{mid_fade_out}");
        assert!(state.fading(&osd, now + Duration::from_millis(1425)));
        assert_eq!(state.opacity(&osd, now + Duration::from_millis(1500)), 0.0);
        assert!(!state.is_visible(now + Duration::from_millis(1500)));
    }

    #[test]
    fn repaints_per_frame_only_while_fading() {
        let osd = OsdSettings::default();
        let opaque = OsdSettings {
            transparent: false,
            ..OsdSettings::default()
        };
        let now = Instant::now();
        let mut state = OsdState::default();
        state.show(
            OsdContent::Volume {
                percent: 40,
                muted: false,
                pending: false,
            },
            Duration::from_millis(1500),
            now,
        );
        let hide_at = now + Duration::from_millis(1500);
        let at = |ms| now + Duration::from_millis(ms);

        // Fading in.
        assert_eq!(state.repaint_delay(&osd, hide_at, now), ANIM_STEP);
        // Fully opaque: sleep until the fade-out is due.
        assert_eq!(
            state.repaint_delay(&osd, hide_at, at(400)),
            Duration::from_millis(1100) - FADE_OUT
        );
        // Fading out.
        assert_eq!(state.repaint_delay(&osd, hide_at, at(1400)), ANIM_STEP);
        assert_eq!(
            state.repaint_delay(&osd, hide_at, at(1495)),
            Duration::from_millis(5)
        );
        // Opaque fallback: exactly one wake-up, at expiry.
        assert_eq!(
            state.repaint_delay(&opaque, hide_at, at(400)),
            Duration::from_millis(1100)
        );
    }

    #[test]
    fn the_opaque_fallback_does_not_fade() {
        let osd = OsdSettings {
            transparent: false,
            ..OsdSettings::default()
        };
        let now = Instant::now();
        let mut state = OsdState::default();
        state.show(
            OsdContent::Volume {
                percent: 40,
                muted: false,
                pending: false,
            },
            Duration::from_millis(1500),
            now,
        );
        assert_eq!(state.opacity(&osd, now), 1.0);
        assert_eq!(state.opacity(&osd, now + Duration::from_millis(1499)), 1.0);
        assert_eq!(state.opacity(&osd, now + Duration::from_millis(1500)), 0.0);
        assert!(!state.fading(&osd, now));
    }

    #[test]
    fn showing_again_after_idle_asks_for_a_reposition() {
        let now = Instant::now();
        let mut state = OsdState::default();
        let content = OsdContent::Volume {
            percent: 10,
            muted: false,
            pending: true,
        };

        state.needs_reposition = false;
        state.show(content.clone(), Duration::from_millis(100), now);
        assert!(state.needs_reposition);

        // Extending a visible popup keeps the placement and the fade-in start.
        state.needs_reposition = false;
        let later = now + Duration::from_millis(50);
        state.show(content.clone(), Duration::from_millis(100), later);
        assert!(!state.needs_reposition);
        assert_eq!(state.shown_at, Some(now));

        // Showing after it expired repositions again.
        let after = now + Duration::from_millis(500);
        state.show(content, Duration::from_millis(100), after);
        assert!(state.needs_reposition);
        assert_eq!(state.shown_at, Some(after));
    }
}
